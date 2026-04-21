#include <stdio.h>
#include <string.h>

#include "../../include/sgm_gc.h"
#include "../../src/json_helpers.h"

int test_json_helpers(void) {
  if (!sgm_json_health_ok("{\"ok\":true}")) {
    printf("health parser mismatch for positive case\\n");
    return 1;
  }
  if (sgm_json_health_ok("{\"ok\":false}")) {
    printf("health parser mismatch for negative case\\n");
    return 1;
  }

  const char* saves_json =
      "{\"success\":true,\"saves\":["
      "{\"id\":\"save-1\",\"displayTitle\":\"Mario Sunshine\",\"filename\":\"mario.gci\",\"version\":3,\"fileSize\":32768,\"createdAt\":\"2026-04-21T10:00:00Z\",\"sha256\":\"a\",\"game\":{\"system\":{\"slug\":\"gamecube\",\"name\":\"Nintendo GameCube\"}}},"
      "{\"id\":\"save-2\",\"displayTitle\":\"Mario Sunshine\",\"filename\":\"mario.gci\",\"version\":2,\"fileSize\":32768,\"createdAt\":\"2026-04-20T10:00:00Z\",\"sha256\":\"b\",\"game\":{\"system\":{\"slug\":\"gamecube\",\"name\":\"Nintendo GameCube\"}}},"
      "{\"id\":\"save-3\",\"displayTitle\":\"SNES Demo\",\"filename\":\"demo.srm\",\"version\":1,\"fileSize\":1024,\"createdAt\":\"2026-04-19T10:00:00Z\",\"sha256\":\"c\",\"game\":{\"system\":{\"slug\":\"snes\",\"name\":\"Super Nintendo\"}}}"
      "]}";

  SgmSaveVersion versions[32];
  int count = sgm_json_parse_saves(saves_json, versions, 32);
  if (count != 3) {
    printf("parse saves count mismatch: %d\n", count);
    return 1;
  }

  SgmBackendGame games[16];
  int game_count = sgm_json_group_gamecube_games(versions, count, games, 16);
  if (game_count != 1) {
    printf("group game count mismatch: %d\n", game_count);
    return 1;
  }

  if (strcmp(games[0].display_title, "Mario Sunshine") != 0) {
    printf("grouped game title mismatch: %s\n", games[0].display_title);
    return 1;
  }

  const char* versions_json =
      "{\"success\":true,\"versions\":["
      "{\"id\":\"save-1\",\"displayTitle\":\"Mario Sunshine\",\"filename\":\"mario.gci\",\"version\":3,\"fileSize\":32768,\"createdAt\":\"2026-04-21T10:00:00Z\",\"sha256\":\"a\"},"
      "{\"id\":\"save-2\",\"displayTitle\":\"Mario Sunshine\",\"filename\":\"mario.gci\",\"version\":2,\"fileSize\":32768,\"createdAt\":\"2026-04-20T10:00:00Z\",\"sha256\":\"b\"}"
      "]}";

  SgmSaveVersion parsed_versions[16];
  int parsed_count = sgm_json_parse_versions(versions_json, parsed_versions, 16);
  if (parsed_count != 2) {
    printf("parse versions count mismatch: %d\n", parsed_count);
    return 1;
  }

  if (parsed_versions[0].version != 3) {
    printf("version parse mismatch: %d\n", parsed_versions[0].version);
    return 1;
  }

  return 0;
}
