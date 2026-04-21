#ifndef SGM_GC_NET_HTTP_H
#define SGM_GC_NET_HTTP_H

#include <stddef.h>

#include "../include/sgm_gc.h"

int sgm_http_get(const char* ip, int port, const char* path, const SgmHttpHeader* headers, int header_count,
                 SgmHttpResponse* out_response, int timeout_ms);
int sgm_http_post_multipart_file(const char* ip, int port, const char* path, const SgmHttpHeader* headers,
                                 int header_count, const char* file_field, const char* file_name,
                                 const unsigned char* file_data, size_t file_len,
                                 const SgmHttpHeader* form_fields, int form_field_count,
                                 SgmHttpResponse* out_response, int timeout_ms);

#endif
