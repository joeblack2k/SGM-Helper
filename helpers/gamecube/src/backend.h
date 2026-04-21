#ifndef SGM_GC_BACKEND_H
#define SGM_GC_BACKEND_H

#include "../include/sgm_gc.h"

int sgm_backend_probe_health(const char* ip, int port);
int sgm_backend_validate_password(const SgmServer* server, const char* compact_password,
                                  const char* fingerprint, char* error, size_t error_size);
int sgm_backend_list_gamecube_games(const SgmServer* server, const char* compact_password,
                                    const char* fingerprint, SgmBackendGame* out_games,
                                    int max_games, SgmSaveVersion* out_all_versions,
                                    int max_versions, int* out_total_versions,
                                    char* error, size_t error_size);
int sgm_backend_fetch_versions(const SgmServer* server, const char* compact_password,
                               const char* fingerprint, const char* save_id,
                               SgmSaveVersion* out_versions, int max_versions,
                               char* error, size_t error_size);
int sgm_backend_download_save(const SgmServer* server, const char* compact_password,
                              const char* fingerprint, const char* save_id,
                              SgmBlob* out_blob, char* error, size_t error_size);
int sgm_backend_upload_gci(const SgmServer* server, const char* compact_password,
                           const char* fingerprint, const char* filename,
                           const unsigned char* gci_data, size_t gci_len,
                           char* error, size_t error_size);

#endif
