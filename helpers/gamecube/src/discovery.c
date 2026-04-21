#include "discovery.h"

#include <arpa/inet.h>
#include <netinet/in.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/select.h>
#include <sys/socket.h>
#include <unistd.h>

#include "backend.h"
#include "platform.h"

static const int k_probe_ports[] = {80, 8080, 9096, 3001};

static int server_exists(const SgmServer* servers, int count, const char* ip, int port) {
  for (int i = 0; i < count; i++) {
    if (servers[i].port == port && strcmp(servers[i].ip, ip) == 0) {
      return 1;
    }
  }
  return 0;
}

static int add_server(SgmServer* servers, int max_servers, int current_count, const char* ip, int port) {
  if (current_count >= max_servers) {
    return current_count;
  }
  if (server_exists(servers, current_count, ip, port)) {
    return current_count;
  }

  SgmServer* server = &servers[current_count];
  memset(server, 0, sizeof(*server));
  snprintf(server->label, sizeof(server->label), "Save Game Manager");
  snprintf(server->ip, sizeof(server->ip), "%s", ip);
  server->port = port;
  return current_count + 1;
}

static int derive_subnet_prefix(char* out_prefix, size_t out_size) {
  char local_ip[64] = {0};
  if (sgm_platform_get_local_ip(local_ip, sizeof(local_ip)) != 0) {
    return -1;
  }

  int a = 0, b = 0, c = 0, d = 0;
  if (sscanf(local_ip, "%d.%d.%d.%d", &a, &b, &c, &d) != 4) {
    return -1;
  }
  snprintf(out_prefix, out_size, "%d.%d.%d", a, b, c);
  return 0;
}

static int mdns_discover_ip_candidates(char ips[][48], int max_ips) {
  if (!ips || max_ips <= 0) {
    return 0;
  }

  int sock = socket(AF_INET, SOCK_DGRAM, 0);
  if (sock < 0) {
    return 0;
  }

  struct timeval timeout;
  timeout.tv_sec = 0;
  timeout.tv_usec = 250000;
  setsockopt(sock, SOL_SOCKET, SO_RCVTIMEO, &timeout, sizeof(timeout));

  unsigned char query[64] = {0};
  size_t q = 0;

  query[q++] = 0x00; query[q++] = 0x00;
  query[q++] = 0x00; query[q++] = 0x00;
  query[q++] = 0x00; query[q++] = 0x01;
  query[q++] = 0x00; query[q++] = 0x00;
  query[q++] = 0x00; query[q++] = 0x00;
  query[q++] = 0x00; query[q++] = 0x00;

  query[q++] = 5;
  memcpy(&query[q], "_http", 5); q += 5;
  query[q++] = 4;
  memcpy(&query[q], "_tcp", 4); q += 4;
  query[q++] = 5;
  memcpy(&query[q], "local", 5); q += 5;
  query[q++] = 0;

  query[q++] = 0x00; query[q++] = 0x0c;
  query[q++] = 0x00; query[q++] = 0x01;

  struct sockaddr_in mdns;
  memset(&mdns, 0, sizeof(mdns));
  mdns.sin_family = AF_INET;
  mdns.sin_port = htons(5353);
  inet_pton(AF_INET, "224.0.0.251", &mdns.sin_addr);

  sendto(sock, query, q, 0, (struct sockaddr*)&mdns, sizeof(mdns));

  int found = 0;
  for (int i = 0; i < 8; i++) {
    fd_set rfds;
    FD_ZERO(&rfds);
    FD_SET(sock, &rfds);

    struct timeval tv;
    tv.tv_sec = 0;
    tv.tv_usec = 120000;

    int rc = select(sock + 1, &rfds, NULL, NULL, &tv);
    if (rc <= 0) {
      continue;
    }

    unsigned char packet[1500];
    struct sockaddr_in from;
    socklen_t from_len = sizeof(from);
    int len = recvfrom(sock, packet, sizeof(packet), 0, (struct sockaddr*)&from, &from_len);
    if (len <= 0) {
      continue;
    }

    char from_ip[48] = {0};
    inet_ntop(AF_INET, &from.sin_addr, from_ip, sizeof(from_ip));
    if (from_ip[0] == '\0') {
      continue;
    }

    int duplicate = 0;
    for (int k = 0; k < found; k++) {
      if (strcmp(ips[k], from_ip) == 0) {
        duplicate = 1;
        break;
      }
    }
    if (duplicate || found >= max_ips) {
      continue;
    }

    snprintf(ips[found++], 48, "%s", from_ip);
  }

  close(sock);
  return found;
}

int sgm_discovery_find_servers(SgmServer* out_servers, int max_servers) {
  if (!out_servers || max_servers <= 0) {
    return 0;
  }
  memset(out_servers, 0, sizeof(SgmServer) * (size_t)max_servers);

  int count = 0;

  char mdns_ips[32][48];
  memset(mdns_ips, 0, sizeof(mdns_ips));
  int mdns_count = mdns_discover_ip_candidates(mdns_ips, 32);

  for (int i = 0; i < mdns_count && count < max_servers; i++) {
    for (size_t p = 0; p < sizeof(k_probe_ports) / sizeof(k_probe_ports[0]); p++) {
      int port = k_probe_ports[p];
      if (sgm_backend_probe_health(mdns_ips[i], port) == 0) {
        count = add_server(out_servers, max_servers, count, mdns_ips[i], port);
        break;
      }
    }
  }

  if (count > 0) {
    return count;
  }

  char prefix[32] = {0};
  if (derive_subnet_prefix(prefix, sizeof(prefix)) != 0) {
    return 0;
  }

  char local_ip[48] = {0};
  sgm_platform_get_local_ip(local_ip, sizeof(local_ip));

  for (int host = 1; host <= 254 && count < max_servers; host++) {
    char ip[48] = {0};
    snprintf(ip, sizeof(ip), "%s.%d", prefix, host);

    if (strcmp(ip, local_ip) == 0) {
      continue;
    }

    for (size_t p = 0; p < sizeof(k_probe_ports) / sizeof(k_probe_ports[0]); p++) {
      int port = k_probe_ports[p];
      if (sgm_backend_probe_health(ip, port) == 0) {
        count = add_server(out_servers, max_servers, count, ip, port);
        break;
      }
    }
  }

  return count;
}
