#include "app.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>

#include "backend.h"
#include "discovery.h"
#include "memory_card.h"
#include "platform.h"
#include "secure_store.h"
#include "ui.h"

#ifdef GAMECUBE_TARGET
#define SGM_SECURE_STORE_PATH "sd:/sgm-gamecube/device_password.dat"
#else
#define SGM_SECURE_STORE_PATH "helpers/gamecube/state/device_password.dat"
#endif

typedef struct {
  SgmServer server;
  char compact_password[8];
  char fingerprint[96];
} Session;

static int ensure_state_dir(void) {
#ifndef GAMECUBE_TARGET
  mkdir("helpers", 0755);
  mkdir("helpers/gamecube", 0755);
  mkdir("helpers/gamecube/state", 0755);
#endif
  return 0;
}

static int pick_server(SgmServer* out_server) {
  if (!out_server) {
    return -1;
  }

  sgm_ui_show_status("Looking for servers...", "Scanning mDNS and local subnet", 0);

  SgmServer servers[SGM_MAX_SERVERS];
  int count = sgm_discovery_find_servers(servers, SGM_MAX_SERVERS);

  if (count <= 0) {
    char manual_ip[48] = {0};
    if (sgm_ui_prompt_manual_ip(manual_ip, sizeof(manual_ip)) != 0) {
      return -1;
    }

    const int ports[] = {80, 8080, 9096, 3001};
    for (size_t i = 0; i < sizeof(ports) / sizeof(ports[0]); i++) {
      if (sgm_backend_probe_health(manual_ip, ports[i]) == 0) {
        memset(out_server, 0, sizeof(*out_server));
        snprintf(out_server->label, sizeof(out_server->label), "Save Game Manager");
        snprintf(out_server->ip, sizeof(out_server->ip), "%s", manual_ip);
        out_server->port = ports[i];
        return 0;
      }
    }

    sgm_ui_show_status("No server", "Could not verify health on manual IP", 1);
    return -1;
  }

  char labels[SGM_MAX_SERVERS][128];
  const char* options[SGM_MAX_SERVERS];
  for (int i = 0; i < count; i++) {
    snprintf(labels[i], sizeof(labels[i]), "%s (%s:%d)",
             servers[i].label, servers[i].ip, servers[i].port);
    options[i] = labels[i];
  }

  int selected = sgm_ui_select_list("Looking for servers", "Select Save Game Manager", options, count);
  if (selected < 0 || selected >= count) {
    return -1;
  }

  *out_server = servers[selected];
  return 0;
}

static int acquire_password(Session* session) {
  if (!session) {
    return -1;
  }

  char error[256] = {0};
  char stored[8] = {0};

  if (sgm_secure_store_load_password(SGM_SECURE_STORE_PATH, session->fingerprint,
                                     stored, sizeof(stored)) == 0) {
    if (sgm_backend_validate_password(&session->server, stored, session->fingerprint,
                                      error, sizeof(error)) == 0) {
      snprintf(session->compact_password, sizeof(session->compact_password), "%s", stored);
      return 0;
    }
  }

  while (1) {
    char entered[16] = {0};
    if (sgm_ui_prompt_password(entered, sizeof(entered)) != 0) {
      return -1;
    }

    if (sgm_backend_validate_password(&session->server, entered, session->fingerprint,
                                      error, sizeof(error)) == 0) {
      snprintf(session->compact_password, sizeof(session->compact_password), "%s", entered);
      sgm_secure_store_save_password(SGM_SECURE_STORE_PATH, session->fingerprint, entered);
      return 0;
    }

    sgm_ui_show_status("Invalid password", error[0] ? error : "Password rejected", 1);
  }
}

static int run_save_per_game(Session* session) {
  SgmLocalGame games[SGM_MAX_LOCAL_GAMES];
  int game_count = sgm_card_list_local_games(0, games, SGM_MAX_LOCAL_GAMES);
  if (game_count <= 0) {
    sgm_ui_show_status("Save per game", "No local memory card saves found", 1);
    return -1;
  }

  char labels[SGM_MAX_LOCAL_GAMES][196];
  const char* options[SGM_MAX_LOCAL_GAMES];
  for (int i = 0; i < game_count; i++) {
    snprintf(labels[i], sizeof(labels[i]), "%s (%d blocks)",
             games[i].display_title, games[i].blocks);
    options[i] = labels[i];
  }

  int selected = sgm_ui_select_list("Save per game", "Select local game", options, game_count);
  if (selected < 0 || selected >= game_count) {
    return 0;
  }

  SgmBlob blob = {0};
  SgmLocalGame meta = {0};
  if (sgm_card_export_gci(0, games[selected].fileno, &blob, &meta) != 0) {
    sgm_ui_show_status("Upload failed", "Could not export GCI from memory card", 1);
    return -1;
  }

  char error[256] = {0};
  const char* upload_name = meta.filename[0] ? meta.filename : games[selected].filename;
  if (sgm_backend_upload_gci(&session->server, session->compact_password, session->fingerprint,
                             upload_name, blob.data, blob.len,
                             error, sizeof(error)) != 0) {
    free(blob.data);
    sgm_ui_show_status("Upload failed", error[0] ? error : "Unknown backend error", 1);
    return -1;
  }

  free(blob.data);
  sgm_ui_show_status("Upload complete", "Save per game synced", 1);
  return 0;
}

static int run_restore_from_backend(Session* session) {
  SgmSaveVersion all_versions[SGM_MAX_VERSIONS];
  SgmBackendGame games[SGM_MAX_BACKEND_GAMES];
  int total_versions = 0;

  char error[256] = {0};
  int game_count = sgm_backend_list_gamecube_games(&session->server, session->compact_password,
                                                   session->fingerprint,
                                                   games, SGM_MAX_BACKEND_GAMES,
                                                   all_versions, SGM_MAX_VERSIONS,
                                                   &total_versions,
                                                   error, sizeof(error));
  if (game_count <= 0) {
    sgm_ui_show_status("Restore from backend", error[0] ? error : "No GameCube saves on backend", 1);
    return -1;
  }

  char game_labels[SGM_MAX_BACKEND_GAMES][196];
  const char* game_options[SGM_MAX_BACKEND_GAMES];
  for (int i = 0; i < game_count; i++) {
    snprintf(game_labels[i], sizeof(game_labels[i]), "%s (%d versions)",
             games[i].display_title, games[i].version_count);
    game_options[i] = game_labels[i];
  }

  int game_idx = sgm_ui_select_list("Restore from backend", "Select backend game", game_options, game_count);
  if (game_idx < 0 || game_idx >= game_count) {
    return 0;
  }

  SgmSaveVersion versions[SGM_MAX_VERSIONS];
  int version_count = sgm_backend_fetch_versions(&session->server, session->compact_password,
                                                 session->fingerprint,
                                                 games[game_idx].latest_save_id,
                                                 versions, SGM_MAX_VERSIONS,
                                                 error, sizeof(error));
  if (version_count <= 0) {
    sgm_ui_show_status("Restore from backend", error[0] ? error : "No versions returned", 1);
    return -1;
  }

  char version_labels[SGM_MAX_VERSIONS][240];
  const char* version_options[SGM_MAX_VERSIONS];
  for (int i = 0; i < version_count; i++) {
    snprintf(version_labels[i], sizeof(version_labels[i]), "v%d  %s  %d bytes",
             versions[i].version, versions[i].created_at, versions[i].file_size);
    version_options[i] = version_labels[i];
  }

  int version_idx = sgm_ui_select_list("Restore from backend", "Select version", version_options, version_count);
  if (version_idx < 0 || version_idx >= version_count) {
    return 0;
  }

  SgmBlob blob = {0};
  if (sgm_backend_download_save(&session->server, session->compact_password,
                                session->fingerprint,
                                versions[version_idx].save_id,
                                &blob, error, sizeof(error)) != 0) {
    sgm_ui_show_status("Restore failed", error[0] ? error : "Download failed", 1);
    return -1;
  }

  char conflict_name[64] = {0};
  int has_match = sgm_card_has_matching_entry(0, blob.data, blob.len, conflict_name, sizeof(conflict_name));
  if (has_match > 0) {
    char question[160];
    snprintf(question, sizeof(question), "Overwrite existing save %s?", conflict_name[0] ? conflict_name : "on card");
    if (!sgm_ui_confirm("Overwrite confirmation", question)) {
      free(blob.data);
      sgm_ui_show_status("Restore canceled", "No changes written to memory card", 1);
      return 0;
    }
  }

  int import_rc = sgm_card_import_gci(0, blob.data, blob.len, 1, conflict_name, sizeof(conflict_name));
  free(blob.data);

  if (import_rc != 0) {
    sgm_ui_show_status("Restore failed", "Could not import selected version", 1);
    return -1;
  }

  sgm_ui_show_status("Restore complete", "Selected backend version restored", 1);
  return 0;
}

int sgm_gamecube_run(void) {
  if (sgm_platform_init() != 0) {
    return 1;
  }
  ensure_state_dir();

  if (sgm_card_init() != 0) {
    sgm_ui_show_status("Startup", "Memory card mount failed (Slot A)", 1);
  }

  Session session;
  memset(&session, 0, sizeof(session));
  if (sgm_platform_get_fingerprint(session.fingerprint, sizeof(session.fingerprint)) != 0) {
    snprintf(session.fingerprint, sizeof(session.fingerprint), "gc-fallback");
  }

  int running = 1;
  while (running) {
    if (pick_server(&session.server) != 0) {
      break;
    }

    if (acquire_password(&session) != 0) {
      continue;
    }

    while (1) {
      const char* menu[2] = {"Save per game", "Restore from backend"};
      int choice = sgm_ui_select_list("Save Game Manager", session.server.ip, menu, 2);
      if (choice == 0) {
        run_save_per_game(&session);
      } else if (choice == 1) {
        run_restore_from_backend(&session);
      } else {
        break;
      }
    }
  }

  sgm_card_shutdown();
  sgm_platform_shutdown();
  return 0;
}
