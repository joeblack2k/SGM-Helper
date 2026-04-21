#ifndef SGM_GC_JSON_HELPERS_H
#define SGM_GC_JSON_HELPERS_H

#include "../include/sgm_gc.h"

int sgm_json_health_ok(const char* body);
int sgm_json_parse_saves(const char* body, SgmSaveVersion* out_versions, int max_versions);
int sgm_json_parse_versions(const char* body, SgmSaveVersion* out_versions, int max_versions);
int sgm_json_group_gamecube_games(const SgmSaveVersion* versions, int version_count,
                                  SgmBackendGame* out_games, int max_games);

#endif
