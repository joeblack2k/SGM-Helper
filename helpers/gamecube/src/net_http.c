#include "net_http.h"

#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <netinet/in.h>
#include <stdarg.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <unistd.h>

#if defined(GAMECUBE_TARGET) || defined(WII_TARGET)
#include <network.h>
#define select net_select
#endif

#define HTTP_REQ_MAX (1024 * 1024)

static int set_nonblocking(int fd, int enable) {
  int flags = fcntl(fd, F_GETFL, 0);
  if (flags < 0) {
    return -1;
  }
  if (enable) {
    flags |= O_NONBLOCK;
  } else {
    flags &= ~O_NONBLOCK;
  }
  return fcntl(fd, F_SETFL, flags);
}

static int connect_with_timeout(const char* ip, int port, int timeout_ms) {
  int fd = socket(AF_INET, SOCK_STREAM, 0);
  if (fd < 0) {
    return -1;
  }

  struct sockaddr_in addr;
  memset(&addr, 0, sizeof(addr));
  addr.sin_family = AF_INET;
  addr.sin_port = htons((uint16_t)port);
  if (inet_pton(AF_INET, ip, &addr.sin_addr) != 1) {
    close(fd);
    return -1;
  }

  set_nonblocking(fd, 1);
  int rc = connect(fd, (struct sockaddr*)&addr, sizeof(addr));
  if (rc == 0) {
    set_nonblocking(fd, 0);
    return fd;
  }

  if (errno != EINPROGRESS) {
    close(fd);
    return -1;
  }

  fd_set wfds;
  FD_ZERO(&wfds);
  FD_SET(fd, &wfds);

  struct timeval tv;
  tv.tv_sec = timeout_ms / 1000;
  tv.tv_usec = (timeout_ms % 1000) * 1000;

  rc = select(fd + 1, NULL, &wfds, NULL, &tv);
  if (rc <= 0) {
    close(fd);
    return -1;
  }

  int so_error = 0;
  socklen_t len = sizeof(so_error);
  getsockopt(fd, SOL_SOCKET, SO_ERROR, &so_error, &len);
  if (so_error != 0) {
    close(fd);
    return -1;
  }

  set_nonblocking(fd, 0);
  return fd;
}

static int send_all(int fd, const unsigned char* data, size_t len) {
  size_t off = 0;
  while (off < len) {
    ssize_t wrote = send(fd, data + off, len - off, 0);
    if (wrote <= 0) {
      return -1;
    }
    off += (size_t)wrote;
  }
  return 0;
}

static int recv_all(int fd, unsigned char* data, size_t max_len, int timeout_ms) {
  size_t off = 0;
  while (off + 1 < max_len) {
    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(fd, &rfds);

    struct timeval tv;
    tv.tv_sec = timeout_ms / 1000;
    tv.tv_usec = (timeout_ms % 1000) * 1000;

    int rc = select(fd + 1, &rfds, NULL, NULL, &tv);
    if (rc == 0) {
      break;
    }
    if (rc < 0) {
      return -1;
    }

    ssize_t got = recv(fd, data + off, max_len - off - 1, 0);
    if (got == 0) {
      break;
    }
    if (got < 0) {
      return -1;
    }
    off += (size_t)got;
  }
  data[off] = '\0';
  return (int)off;
}

static int parse_http_response(const unsigned char* raw, int raw_len, SgmHttpResponse* out) {
  if (!raw || raw_len <= 0 || !out) {
    return -1;
  }

  memset(out, 0, sizeof(*out));

  int status = 0;
  sscanf((const char*)raw, "HTTP/%*d.%*d %d", &status);
  out->status_code = status;

  const unsigned char* body = (const unsigned char*)strstr((const char*)raw, "\r\n\r\n");
  if (!body) {
    out->body[0] = '\0';
    out->body_len = 0;
    return 0;
  }

  body += 4;
  int body_len = raw_len - (int)(body - raw);
  if (body_len < 0) {
    body_len = 0;
  }
  if (body_len >= (int)sizeof(out->body)) {
    body_len = (int)sizeof(out->body) - 1;
  }
  memcpy(out->body, body, (size_t)body_len);
  out->body[body_len] = '\0';
  out->body_len = body_len;
  return 0;
}

static int addf(char* dst, size_t dst_size, size_t* offset, const char* fmt, ...) {
  if (!dst || !offset || *offset >= dst_size) {
    return -1;
  }

  va_list args;
  va_start(args, fmt);
  int wrote = vsnprintf(dst + *offset, dst_size - *offset, fmt, args);
  va_end(args);
  if (wrote < 0) {
    return -1;
  }
  if ((size_t)wrote >= dst_size - *offset) {
    return -1;
  }
  *offset += (size_t)wrote;
  return 0;
}

int sgm_http_get(const char* ip, int port, const char* path, const SgmHttpHeader* headers,
                 int header_count, SgmHttpResponse* out_response, int timeout_ms) {
  if (!ip || !path || !out_response) {
    return -1;
  }

  int fd = connect_with_timeout(ip, port, timeout_ms);
  if (fd < 0) {
    return -1;
  }

  char req[8192];
  size_t off = 0;
  if (addf(req, sizeof(req), &off, "GET %s HTTP/1.1\r\n", path) != 0 ||
      addf(req, sizeof(req), &off, "Host: %s:%d\r\n", ip, port) != 0 ||
      addf(req, sizeof(req), &off, "Connection: close\r\n") != 0 ||
      addf(req, sizeof(req), &off, "Accept: application/json\r\n") != 0) {
    close(fd);
    return -1;
  }

  for (int i = 0; i < header_count; i++) {
    if (addf(req, sizeof(req), &off, "%s: %s\r\n", headers[i].key, headers[i].value) != 0) {
      close(fd);
      return -1;
    }
  }

  if (addf(req, sizeof(req), &off, "\r\n") != 0) {
    close(fd);
    return -1;
  }

  if (send_all(fd, (const unsigned char*)req, off) != 0) {
    close(fd);
    return -1;
  }

  unsigned char* raw = (unsigned char*)malloc(SGM_MAX_HTTP_BODY + 8192);
  if (!raw) {
    close(fd);
    return -1;
  }

  int raw_len = recv_all(fd, raw, SGM_MAX_HTTP_BODY + 8192, timeout_ms);
  close(fd);
  if (raw_len < 0) {
    free(raw);
    return -1;
  }

  int rc = parse_http_response(raw, raw_len, out_response);
  free(raw);
  return rc;
}

int sgm_http_post_multipart_file(const char* ip, int port, const char* path, const SgmHttpHeader* headers,
                                 int header_count, const char* file_field, const char* file_name,
                                 const unsigned char* file_data, size_t file_len,
                                 const SgmHttpHeader* form_fields, int form_field_count,
                                 SgmHttpResponse* out_response, int timeout_ms) {
  if (!ip || !path || !file_field || !file_name || !file_data || !out_response) {
    return -1;
  }

  int fd = connect_with_timeout(ip, port, timeout_ms);
  if (fd < 0) {
    return -1;
  }

  const char* boundary = "----SGMGCBOUNDARY7f38f2a";

  size_t body_cap = HTTP_REQ_MAX;
  unsigned char* body = (unsigned char*)malloc(body_cap);
  if (!body) {
    close(fd);
    return -1;
  }

  size_t body_len = 0;
  char line[512];

  for (int i = 0; i < form_field_count; i++) {
    int n = snprintf(line, sizeof(line),
                     "--%s\r\nContent-Disposition: form-data; name=\"%s\"\r\n\r\n%s\r\n",
                     boundary, form_fields[i].key, form_fields[i].value);
    if (n <= 0 || body_len + (size_t)n >= body_cap) {
      free(body);
      close(fd);
      return -1;
    }
    memcpy(body + body_len, line, (size_t)n);
    body_len += (size_t)n;
  }

  int n = snprintf(line, sizeof(line),
                   "--%s\r\nContent-Disposition: form-data; name=\"%s\"; filename=\"%s\"\r\n"
                   "Content-Type: application/octet-stream\r\n\r\n",
                   boundary, file_field, file_name);
  if (n <= 0 || body_len + (size_t)n + file_len + 128 >= body_cap) {
    free(body);
    close(fd);
    return -1;
  }
  memcpy(body + body_len, line, (size_t)n);
  body_len += (size_t)n;

  memcpy(body + body_len, file_data, file_len);
  body_len += file_len;

  n = snprintf(line, sizeof(line), "\r\n--%s--\r\n", boundary);
  if (n <= 0 || body_len + (size_t)n >= body_cap) {
    free(body);
    close(fd);
    return -1;
  }
  memcpy(body + body_len, line, (size_t)n);
  body_len += (size_t)n;

  char req_head[8192];
  size_t off = 0;
  if (addf(req_head, sizeof(req_head), &off, "POST %s HTTP/1.1\r\n", path) != 0 ||
      addf(req_head, sizeof(req_head), &off, "Host: %s:%d\r\n", ip, port) != 0 ||
      addf(req_head, sizeof(req_head), &off, "Connection: close\r\n") != 0 ||
      addf(req_head, sizeof(req_head), &off, "Content-Type: multipart/form-data; boundary=%s\r\n", boundary) != 0 ||
      addf(req_head, sizeof(req_head), &off, "Content-Length: %zu\r\n", body_len) != 0) {
    free(body);
    close(fd);
    return -1;
  }

  for (int i = 0; i < header_count; i++) {
    if (addf(req_head, sizeof(req_head), &off, "%s: %s\r\n", headers[i].key, headers[i].value) != 0) {
      free(body);
      close(fd);
      return -1;
    }
  }

  if (addf(req_head, sizeof(req_head), &off, "\r\n") != 0) {
    free(body);
    close(fd);
    return -1;
  }

  if (send_all(fd, (const unsigned char*)req_head, off) != 0 ||
      send_all(fd, body, body_len) != 0) {
    free(body);
    close(fd);
    return -1;
  }
  free(body);

  unsigned char* raw = (unsigned char*)malloc(SGM_MAX_HTTP_BODY + 8192);
  if (!raw) {
    close(fd);
    return -1;
  }

  int raw_len = recv_all(fd, raw, SGM_MAX_HTTP_BODY + 8192, timeout_ms);
  close(fd);
  if (raw_len < 0) {
    free(raw);
    return -1;
  }

  int rc = parse_http_response(raw, raw_len, out_response);
  free(raw);
  return rc;
}
