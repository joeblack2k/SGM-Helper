#include "backend.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <stdarg.h>

#include "json_helpers.h"
#include "net_http.h"

static void set_error(char* error, size_t error_size, const char* fmt, ...) {
  if (!error || error_size == 0) {
    return;
  }

  va_list args;
  va_start(args, fmt);
  vsnprintf(error, error_size, fmt, args);
  va_end(args);
}

int sgm_backend_probe_health(const char* ip, int port) {
  SgmHttpResponse response;
  if (sgm_http_get(ip, port, "/healthz", NULL, 0, &response, 800) != 0) {
    return -1;
  }
  if (response.status_code != 200) {
    return -1;
  }
  return sgm_json_health_ok(response.body) ? 0 : -1;
}

static int helper_headers(const char* compact_password, const char* fingerprint,
                          SgmHttpHeader* out_headers, int max_headers) {
  if (!compact_password || !fingerprint || !out_headers || max_headers < 3) {
    return 0;
  }
  snprintf(out_headers[0].key, sizeof(out_headers[0].key), "X-RSM-App-Password");
  snprintf(out_headers[0].value, sizeof(out_headers[0].value), "%s", compact_password);

  snprintf(out_headers[1].key, sizeof(out_headers[1].key), "X-RSM-Device-Type");
  snprintf(out_headers[1].value, sizeof(out_headers[1].value), "%s", SGM_DEVICE_TYPE);

  snprintf(out_headers[2].key, sizeof(out_headers[2].key), "X-RSM-Fingerprint");
  snprintf(out_headers[2].value, sizeof(out_headers[2].value), "%s", fingerprint);

  return 3;
}

int sgm_backend_validate_password(const SgmServer* server, const char* compact_password,
                                  const char* fingerprint, char* error, size_t error_size) {
  if (!server || !compact_password || !fingerprint) {
    set_error(error, error_size, "missing inputs");
    return -1;
  }

  SgmHttpHeader headers[3];
  int header_count = helper_headers(compact_password, fingerprint, headers, 3);

  SgmHttpResponse response;
  if (sgm_http_get(server->ip, server->port,
                   "/save/latest?romSha1=sgm-probe&slotName=card-a",
                   headers, header_count, &response, 1200) != 0) {
    set_error(error, error_size, "network error while validating password");
    return -1;
  }

  if (response.status_code == 200) {
    return 0;
  }

  set_error(error, error_size, "backend rejected password (HTTP %d): %.120s",
            response.status_code, response.body);
  return -1;
}

int sgm_backend_list_gamecube_games(const SgmServer* server, const char* compact_password,
                                    const char* fingerprint, SgmBackendGame* out_games,
                                    int max_games, SgmSaveVersion* out_all_versions,
                                    int max_versions, int* out_total_versions,
                                    char* error, size_t error_size) {
  if (!server || !out_games || !out_all_versions || max_games <= 0 || max_versions <= 0) {
    set_error(error, error_size, "invalid arguments");
    return -1;
  }

  SgmHttpHeader headers[3];
  int header_count = helper_headers(compact_password, fingerprint, headers, 3);

  SgmHttpResponse response;
  if (sgm_http_get(server->ip, server->port, "/saves?limit=500&offset=0",
                   headers, header_count, &response, 1500) != 0) {
    set_error(error, error_size, "network error while loading saves");
    return -1;
  }

  if (response.status_code != 200) {
    set_error(error, error_size, "failed to load saves (HTTP %d)", response.status_code);
    return -1;
  }

  int version_count = sgm_json_parse_saves(response.body, out_all_versions, max_versions);
  if (version_count < 0) {
    set_error(error, error_size, "could not parse saves payload");
    return -1;
  }
  if (out_total_versions) {
    *out_total_versions = version_count;
  }

  int game_count = sgm_json_group_gamecube_games(out_all_versions, version_count, out_games, max_games);
  if (game_count < 0) {
    set_error(error, error_size, "could not group gamecube games");
    return -1;
  }

  return game_count;
}

int sgm_backend_fetch_versions(const SgmServer* server, const char* compact_password,
                               const char* fingerprint, const char* save_id,
                               SgmSaveVersion* out_versions, int max_versions,
                               char* error, size_t error_size) {
  if (!server || !save_id || !out_versions || max_versions <= 0) {
    set_error(error, error_size, "invalid arguments");
    return -1;
  }

  char path[256];
  snprintf(path, sizeof(path), "/save?saveId=%s", save_id);

  SgmHttpHeader headers[3];
  int header_count = helper_headers(compact_password, fingerprint, headers, 3);

  SgmHttpResponse response;
  if (sgm_http_get(server->ip, server->port, path, headers, header_count,
                   &response, 1500) != 0) {
    set_error(error, error_size, "network error while loading versions");
    return -1;
  }
  if (response.status_code != 200) {
    set_error(error, error_size, "failed to load versions (HTTP %d)", response.status_code);
    return -1;
  }

  int count = sgm_json_parse_versions(response.body, out_versions, max_versions);
  if (count < 0) {
    set_error(error, error_size, "could not parse versions payload");
    return -1;
  }
  return count;
}

int sgm_backend_download_save(const SgmServer* server, const char* compact_password,
                              const char* fingerprint, const char* save_id,
                              SgmBlob* out_blob, char* error, size_t error_size) {
  if (!server || !save_id || !out_blob) {
    set_error(error, error_size, "invalid arguments");
    return -1;
  }

  char path[256];
  snprintf(path, sizeof(path), "/saves/download?id=%s", save_id);

  SgmHttpHeader headers[3];
  int header_count = helper_headers(compact_password, fingerprint, headers, 3);

  SgmHttpResponse response;
  if (sgm_http_get(server->ip, server->port, path, headers, header_count,
                   &response, 2000) != 0) {
    set_error(error, error_size, "network error while downloading save");
    return -1;
  }

  if (response.status_code != 200 || response.body_len <= 0) {
    set_error(error, error_size, "download failed (HTTP %d)", response.status_code);
    return -1;
  }

  out_blob->data = (unsigned char*)malloc((size_t)response.body_len);
  if (!out_blob->data) {
    set_error(error, error_size, "out of memory");
    return -1;
  }
  out_blob->len = (size_t)response.body_len;
  memcpy(out_blob->data, response.body, out_blob->len);
  return 0;
}

int sgm_backend_upload_gci(const SgmServer* server, const char* compact_password,
                           const char* fingerprint, const char* filename,
                           const unsigned char* gci_data, size_t gci_len,
                           char* error, size_t error_size) {
  if (!server || !filename || !gci_data || gci_len == 0) {
    set_error(error, error_size, "invalid upload inputs");
    return -1;
  }

  SgmHttpHeader headers[4];
  int header_count = helper_headers(compact_password, fingerprint, headers, 4);
  snprintf(headers[header_count].key, sizeof(headers[header_count].key), "X-CSRF-Protection");
  snprintf(headers[header_count].value, sizeof(headers[header_count].value), "1");
  header_count++;

  SgmHttpHeader form_fields[2];
  snprintf(form_fields[0].key, sizeof(form_fields[0].key), "system");
  snprintf(form_fields[0].value, sizeof(form_fields[0].value), "gamecube");
  snprintf(form_fields[1].key, sizeof(form_fields[1].key), "slotName");
  snprintf(form_fields[1].value, sizeof(form_fields[1].value), SGM_DEFAULT_SLOT_NAME);

  SgmHttpResponse response;
  if (sgm_http_post_multipart_file(server->ip, server->port, "/saves",
                                   headers, header_count,
                                   "file", filename,
                                   gci_data, gci_len,
                                   form_fields, 2,
                                   &response, 3000) != 0) {
    set_error(error, error_size, "network error while uploading save");
    return -1;
  }

  if (response.status_code != 200) {
    set_error(error, error_size, "upload failed (HTTP %d): %.120s",
              response.status_code, response.body);
    return -1;
  }

  return 0;
}
