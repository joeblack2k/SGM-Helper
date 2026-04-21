#ifndef SGM_GC_SECURE_STORE_H
#define SGM_GC_SECURE_STORE_H

#include <stddef.h>

int sgm_password_normalize(const char* raw, char* out_compact, size_t out_size);
void sgm_password_format(const char* compact, char* out_formatted, size_t out_size);
int sgm_secure_store_save_password(const char* path, const char* fingerprint,
                                   const char* compact_password);
int sgm_secure_store_load_password(const char* path, const char* fingerprint,
                                   char* out_compact_password, size_t out_size);

#endif
