#ifndef SGM_GC_MEMORY_CARD_H
#define SGM_GC_MEMORY_CARD_H

#include "../include/sgm_gc.h"

int sgm_card_init(void);
void sgm_card_shutdown(void);
int sgm_card_list_local_games(int slot, SgmLocalGame* out_games, int max_games);
int sgm_card_export_gci(int slot, int fileno, SgmBlob* out_blob, SgmLocalGame* out_meta);
int sgm_card_import_gci(int slot, const unsigned char* gci_data, size_t gci_len,
                        int overwrite_existing, char* conflict_name, size_t conflict_name_size);
int sgm_card_has_matching_entry(int slot, const unsigned char* gci_data, size_t gci_len,
                                char* out_name, size_t out_name_size);

#endif
