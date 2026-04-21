#include "memory_card.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#if defined(GAMECUBE_TARGET) || defined(WII_TARGET)

#include <malloc.h>
#include <ogc/card.h>

#define SGM_CARD_SLOT 0

static unsigned char g_card_work_area[32768] __attribute__((aligned(32)));
static int g_card_mounted = 0;

static size_t align32(size_t value) {
  return (value + 31u) & ~(size_t)31u;
}

int sgm_card_init(void) {
  CARD_Init("", "");

  int card_size = 0;
  int sector_size = 0;
  if (CARD_ProbeEx(SGM_CARD_SLOT, &card_size, &sector_size) < 0) {
    return -1;
  }

  if (CARD_Mount(SGM_CARD_SLOT, g_card_work_area, NULL) < 0) {
    return -1;
  }

  g_card_mounted = 1;
  return 0;
}

void sgm_card_shutdown(void) {
  if (g_card_mounted) {
    CARD_Unmount(SGM_CARD_SLOT);
  }
  g_card_mounted = 0;
}

static int card_find_dir_by_fileno(int fileno, card_dir* out_dir) {
  card_dir dirs[127];
  int count = 127;
  if (CARD_GetDirectory(SGM_CARD_SLOT, dirs, &count, true) < 0) {
    return -1;
  }

  for (int i = 0; i < count; i++) {
    if ((int)dirs[i].fileno == fileno) {
      if (out_dir) {
        *out_dir = dirs[i];
      }
      return 0;
    }
  }
  return -1;
}

int sgm_card_list_local_games(int slot, SgmLocalGame* out_games, int max_games) {
  (void)slot;
  if (!g_card_mounted || !out_games || max_games <= 0) {
    return -1;
  }

  card_dir dirs[127];
  int count = 127;
  if (CARD_GetDirectory(SGM_CARD_SLOT, dirs, &count, true) < 0) {
    return -1;
  }

  int out_count = 0;
  for (int i = 0; i < count && out_count < max_games; i++) {
    SgmLocalGame* game = &out_games[out_count++];
    memset(game, 0, sizeof(*game));

    game->fileno = (int)dirs[i].fileno;
    snprintf(game->filename, sizeof(game->filename), "%.*s", CARD_FILENAMELEN, dirs[i].filename);
    snprintf(game->display_title, sizeof(game->display_title), "%.*s", CARD_FILENAMELEN,
             dirs[i].filename);

    memcpy(game->gamecode, dirs[i].gamecode, 4);
    game->gamecode[4] = '\0';

    memcpy(game->company, dirs[i].company, 2);
    game->company[2] = '\0';

    game->file_size = (int)dirs[i].filelen;
    game->blocks = (game->file_size + 8191) / 8192;
  }

  return out_count;
}

int sgm_card_export_gci(int slot, int fileno, SgmBlob* out_blob, SgmLocalGame* out_meta) {
  (void)slot;
  if (!g_card_mounted || !out_blob) {
    return -1;
  }

  out_blob->data = NULL;
  out_blob->len = 0;

  card_dir dir;
  if (card_find_dir_by_fileno(fileno, &dir) != 0) {
    return -1;
  }

  card_file file;
  if (CARD_OpenEntry(SGM_CARD_SLOT, &dir, &file) < 0) {
    return -1;
  }

  card_direntry entry;
  memset(&entry, 0, sizeof(entry));
  if (CARD_GetStatusEx(SGM_CARD_SLOT, file.filenum, &entry) < 0) {
    CARD_Close(&file);
    return -1;
  }

  size_t payload_len = (size_t)file.len;
  size_t payload_aligned = align32(payload_len > 0 ? payload_len : 32);
  unsigned char* payload = (unsigned char*)memalign(32, payload_aligned);
  if (!payload) {
    CARD_Close(&file);
    return -1;
  }
  memset(payload, 0, payload_aligned);

  if (CARD_Read(&file, payload, (u32)payload_len, 0) < 0) {
    free(payload);
    CARD_Close(&file);
    return -1;
  }

  size_t total_len = sizeof(card_direntry) + payload_len;
  unsigned char* gci = (unsigned char*)malloc(total_len);
  if (!gci) {
    free(payload);
    CARD_Close(&file);
    return -1;
  }

  memcpy(gci, &entry, sizeof(card_direntry));
  memcpy(gci + sizeof(card_direntry), payload, payload_len);
  free(payload);

  CARD_Close(&file);

  out_blob->data = gci;
  out_blob->len = total_len;

  if (out_meta) {
    memset(out_meta, 0, sizeof(*out_meta));
    out_meta->fileno = fileno;
    snprintf(out_meta->filename, sizeof(out_meta->filename), "%.*s", CARD_FILENAMELEN, dir.filename);
    snprintf(out_meta->display_title, sizeof(out_meta->display_title), "%.*s",
             CARD_FILENAMELEN, dir.filename);
    memcpy(out_meta->gamecode, dir.gamecode, 4);
    out_meta->gamecode[4] = '\0';
    memcpy(out_meta->company, dir.company, 2);
    out_meta->company[2] = '\0';
    out_meta->file_size = file.len;
    out_meta->blocks = (file.len + 8191) / 8192;
  }

  return 0;
}

static int find_matching_entry(const card_direntry* target, card_dir* out_match) {
  card_dir dirs[127];
  int count = 127;
  if (CARD_GetDirectory(SGM_CARD_SLOT, dirs, &count, true) < 0) {
    return -1;
  }

  for (int i = 0; i < count; i++) {
    if (strncmp((const char*)dirs[i].filename, (const char*)target->filename, CARD_FILENAMELEN) != 0) {
      continue;
    }
    if (memcmp(dirs[i].gamecode, target->gamecode, 4) != 0) {
      continue;
    }
    if (memcmp(dirs[i].company, target->company, 2) != 0) {
      continue;
    }

    if (out_match) {
      *out_match = dirs[i];
    }
    return 0;
  }

  return -1;
}

int sgm_card_has_matching_entry(int slot, const unsigned char* gci_data, size_t gci_len,
                                char* out_name, size_t out_name_size) {
  (void)slot;
  if (!g_card_mounted || !gci_data || gci_len <= sizeof(card_direntry)) {
    return -1;
  }

  const card_direntry* header = (const card_direntry*)gci_data;
  card_dir existing;
  if (find_matching_entry(header, &existing) != 0) {
    return 0;
  }

  if (out_name && out_name_size > 0) {
    snprintf(out_name, out_name_size, "%.*s", CARD_FILENAMELEN, existing.filename);
  }
  return 1;
}

int sgm_card_import_gci(int slot, const unsigned char* gci_data, size_t gci_len,
                        int overwrite_existing, char* conflict_name, size_t conflict_name_size) {
  (void)slot;
  if (!g_card_mounted || !gci_data || gci_len <= sizeof(card_direntry)) {
    return -1;
  }

  const card_direntry* header = (const card_direntry*)gci_data;
  const unsigned char* payload = gci_data + sizeof(card_direntry);
  size_t payload_len = gci_len - sizeof(card_direntry);

  card_dir existing;
  int has_existing = find_matching_entry(header, &existing) == 0;
  if (has_existing && !overwrite_existing) {
    if (conflict_name && conflict_name_size > 0) {
      snprintf(conflict_name, conflict_name_size, "%.*s", CARD_FILENAMELEN, existing.filename);
    }
    return 1;
  }

  if (has_existing) {
    if (CARD_DeleteEntry(SGM_CARD_SLOT, &existing) < 0) {
      return -1;
    }
  }

  card_dir new_entry;
  memset(&new_entry, 0, sizeof(new_entry));
  new_entry.chn = SGM_CARD_SLOT;
  new_entry.filelen = (u32)payload_len;
  new_entry.permissions = header->permission;
  memcpy(new_entry.filename, header->filename, CARD_FILENAMELEN);
  memcpy(new_entry.gamecode, header->gamecode, sizeof(new_entry.gamecode));
  memcpy(new_entry.company, header->company, sizeof(new_entry.company));
  new_entry.showall = true;

  card_file file;
  if (CARD_CreateEntry(SGM_CARD_SLOT, &new_entry, &file) < 0) {
    return -1;
  }

  size_t payload_aligned = align32(payload_len > 0 ? payload_len : 32);
  unsigned char* write_buffer = (unsigned char*)memalign(32, payload_aligned);
  if (!write_buffer) {
    CARD_Close(&file);
    return -1;
  }
  memset(write_buffer, 0, payload_aligned);
  memcpy(write_buffer, payload, payload_len);

  if (CARD_Write(&file, write_buffer, (u32)payload_len, 0) < 0) {
    free(write_buffer);
    CARD_Close(&file);
    return -1;
  }
  free(write_buffer);

  card_direntry new_status = *header;
  CARD_SetStatusEx(SGM_CARD_SLOT, file.filenum, &new_status);
  CARD_Close(&file);

  return 0;
}

#else

int sgm_card_init(void) { return 0; }

void sgm_card_shutdown(void) {}

int sgm_card_list_local_games(int slot, SgmLocalGame* out_games, int max_games) {
  (void)slot;
  if (!out_games || max_games < 2) {
    return -1;
  }

  memset(out_games, 0, sizeof(SgmLocalGame) * (size_t)max_games);
  out_games[0].fileno = 1;
  snprintf(out_games[0].filename, sizeof(out_games[0].filename), "mario_sunshine.gci");
  snprintf(out_games[0].display_title, sizeof(out_games[0].display_title), "Mario Sunshine");
  snprintf(out_games[0].gamecode, sizeof(out_games[0].gamecode), "GMSE");
  snprintf(out_games[0].company, sizeof(out_games[0].company), "01");
  out_games[0].file_size = 32768;
  out_games[0].blocks = 4;

  out_games[1].fileno = 2;
  snprintf(out_games[1].filename, sizeof(out_games[1].filename), "zelda_wind_waker.gci");
  snprintf(out_games[1].display_title, sizeof(out_games[1].display_title), "Zelda Wind Waker");
  snprintf(out_games[1].gamecode, sizeof(out_games[1].gamecode), "GZLE");
  snprintf(out_games[1].company, sizeof(out_games[1].company), "01");
  out_games[1].file_size = 65536;
  out_games[1].blocks = 8;

  return 2;
}

int sgm_card_export_gci(int slot, int fileno, SgmBlob* out_blob, SgmLocalGame* out_meta) {
  (void)slot;
  if (!out_blob) {
    return -1;
  }

  const char* sample = "GCI_SAMPLE_BYTES";
  out_blob->len = strlen(sample);
  out_blob->data = (unsigned char*)malloc(out_blob->len);
  if (!out_blob->data) {
    return -1;
  }
  memcpy(out_blob->data, sample, out_blob->len);

  if (out_meta) {
    memset(out_meta, 0, sizeof(*out_meta));
    out_meta->fileno = fileno;
    snprintf(out_meta->filename, sizeof(out_meta->filename), "sample_%d.gci", fileno);
    snprintf(out_meta->display_title, sizeof(out_meta->display_title), "Sample Save");
  }

  return 0;
}

int sgm_card_has_matching_entry(int slot, const unsigned char* gci_data, size_t gci_len,
                                char* out_name, size_t out_name_size) {
  (void)slot;
  (void)gci_data;
  (void)gci_len;
  if (out_name && out_name_size > 0) {
    snprintf(out_name, out_name_size, "sample_match");
  }
  return 1;
}

int sgm_card_import_gci(int slot, const unsigned char* gci_data, size_t gci_len,
                        int overwrite_existing, char* conflict_name, size_t conflict_name_size) {
  (void)slot;
  (void)gci_data;
  (void)gci_len;
  if (!overwrite_existing) {
    if (conflict_name && conflict_name_size > 0) {
      snprintf(conflict_name, conflict_name_size, "sample_match");
    }
    return 1;
  }
  return 0;
}

#endif
