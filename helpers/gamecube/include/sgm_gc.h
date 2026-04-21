#ifndef SGM_GC_H
#define SGM_GC_H

#include <stddef.h>

#define SGM_MAX_SERVERS 16
#define SGM_MAX_LOCAL_GAMES 128
#define SGM_MAX_BACKEND_GAMES 128
#define SGM_MAX_VERSIONS 128
#define SGM_MAX_HTTP_BODY (1024 * 1024)
#define SGM_PASSWORD_COMPACT_LEN 6
#define SGM_PASSWORD_FORMATTED_LEN 7
#define SGM_DEVICE_TYPE "gamecube-swiss"
#define SGM_DEFAULT_SLOT_NAME "card-a"

typedef struct {
  char label[64];
  char ip[48];
  int port;
} SgmServer;

typedef struct {
  int fileno;
  char filename[33];
  char display_title[128];
  char gamecode[5];
  char company[3];
  int file_size;
  int blocks;
} SgmLocalGame;

typedef struct {
  char save_id[96];
  char display_title[128];
  char system_slug[48];
  char system_name[64];
  char filename[96];
  int version;
  int file_size;
  char created_at[40];
  char sha256[80];
} SgmSaveVersion;

typedef struct {
  char game_key[160];
  char display_title[128];
  char system_slug[48];
  char latest_save_id[96];
  int latest_version;
  int version_count;
  int total_size;
} SgmBackendGame;

typedef struct {
  unsigned char* data;
  size_t len;
} SgmBlob;

typedef struct {
  char key[64];
  char value[160];
} SgmHttpHeader;

typedef struct {
  int status_code;
  int body_len;
  char body[SGM_MAX_HTTP_BODY];
} SgmHttpResponse;

typedef struct {
  int up;
  int down;
  int left;
  int right;
  int accept;
  int back;
  int rescan;
  int quit;
} SgmInput;

#endif
