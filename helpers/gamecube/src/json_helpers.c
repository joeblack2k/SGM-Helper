#include "json_helpers.h"

#include <ctype.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

static int is_json_ws(char c) {
  return c == ' ' || c == '\n' || c == '\r' || c == '\t';
}

static const char* skip_ws(const char* p, const char* end) {
  while (p && p < end && is_json_ws(*p)) {
    p++;
  }
  return p;
}

static const char* find_key_in_span(const char* start, const char* end, const char* key) {
  if (!start || !end || !key) {
    return NULL;
  }

  char needle[96];
  snprintf(needle, sizeof(needle), "\"%s\"", key);

  const char* p = start;
  size_t needle_len = strlen(needle);
  while (p && p + needle_len < end) {
    const char* hit = strstr(p, needle);
    if (!hit || hit >= end) {
      return NULL;
    }
    const char* colon = hit + needle_len;
    colon = skip_ws(colon, end);
    if (colon < end && *colon == ':') {
      colon++;
      return skip_ws(colon, end);
    }
    p = hit + needle_len;
  }
  return NULL;
}

static int json_string_at(const char* value, const char* end, char* out, size_t out_size) {
  if (!value || value >= end || *value != '"' || !out || out_size == 0) {
    return -1;
  }

  value++;
  size_t w = 0;
  int escaped = 0;
  for (const char* p = value; p < end; p++) {
    char ch = *p;
    if (escaped) {
      if (w + 1 < out_size) {
        out[w++] = ch;
      }
      escaped = 0;
      continue;
    }
    if (ch == '\\') {
      escaped = 1;
      continue;
    }
    if (ch == '"') {
      out[w] = '\0';
      return 0;
    }
    if (w + 1 < out_size) {
      out[w++] = ch;
    }
  }

  out[0] = '\0';
  return -1;
}

static int json_int_at(const char* value, const char* end, int* out) {
  if (!value || !out || value >= end) {
    return -1;
  }

  char tmp[32];
  size_t w = 0;
  const char* p = value;
  if (*p == '-') {
    tmp[w++] = *p++;
  }
  while (p < end && *p >= '0' && *p <= '9' && w + 1 < sizeof(tmp)) {
    tmp[w++] = *p++;
  }
  if (w == 0 || (w == 1 && tmp[0] == '-')) {
    return -1;
  }
  tmp[w] = '\0';
  *out = atoi(tmp);
  return 0;
}

static int find_array_span(const char* body, const char* key,
                           const char** out_array_start, const char** out_array_end) {
  if (!body || !key || !out_array_start || !out_array_end) {
    return -1;
  }

  const char* start = body;
  const char* end = body + strlen(body);
  const char* value = find_key_in_span(start, end, key);
  if (!value || value >= end || *value != '[') {
    return -1;
  }

  int depth = 0;
  int in_string = 0;
  int escaped = 0;
  const char* p = value;
  for (; p < end; p++) {
    char ch = *p;
    if (in_string) {
      if (escaped) {
        escaped = 0;
      } else if (ch == '\\') {
        escaped = 1;
      } else if (ch == '"') {
        in_string = 0;
      }
      continue;
    }

    if (ch == '"') {
      in_string = 1;
      continue;
    }

    if (ch == '[') {
      depth++;
    } else if (ch == ']') {
      depth--;
      if (depth == 0) {
        *out_array_start = value + 1;
        *out_array_end = p;
        return 0;
      }
    }
  }

  return -1;
}

static int parse_save_object(const char* obj_start, const char* obj_end, SgmSaveVersion* out) {
  if (!obj_start || !obj_end || !out) {
    return -1;
  }

  memset(out, 0, sizeof(*out));

  const char* value;
  value = find_key_in_span(obj_start, obj_end, "id");
  if (value) json_string_at(value, obj_end, out->save_id, sizeof(out->save_id));

  value = find_key_in_span(obj_start, obj_end, "displayTitle");
  if (value) json_string_at(value, obj_end, out->display_title, sizeof(out->display_title));

  if (out->display_title[0] == '\0') {
    value = find_key_in_span(obj_start, obj_end, "name");
    if (value) json_string_at(value, obj_end, out->display_title, sizeof(out->display_title));
  }

  value = find_key_in_span(obj_start, obj_end, "filename");
  if (value) json_string_at(value, obj_end, out->filename, sizeof(out->filename));

  value = find_key_in_span(obj_start, obj_end, "version");
  if (value) json_int_at(value, obj_end, &out->version);

  value = find_key_in_span(obj_start, obj_end, "fileSize");
  if (value) json_int_at(value, obj_end, &out->file_size);

  value = find_key_in_span(obj_start, obj_end, "createdAt");
  if (value) json_string_at(value, obj_end, out->created_at, sizeof(out->created_at));

  value = find_key_in_span(obj_start, obj_end, "sha256");
  if (value) json_string_at(value, obj_end, out->sha256, sizeof(out->sha256));

  const char* system_obj = find_key_in_span(obj_start, obj_end, "system");
  if (system_obj && *system_obj == '{') {
    int depth = 0;
    int in_string = 0;
    int escaped = 0;
    const char* p = system_obj;
    for (; p < obj_end; p++) {
      char ch = *p;
      if (in_string) {
        if (escaped) escaped = 0;
        else if (ch == '\\') escaped = 1;
        else if (ch == '"') in_string = 0;
        continue;
      }
      if (ch == '"') {
        in_string = 1;
        continue;
      }
      if (ch == '{') depth++;
      else if (ch == '}') {
        depth--;
        if (depth == 0) break;
      }
    }

    if (p < obj_end) {
      const char* sub = find_key_in_span(system_obj, p, "slug");
      if (sub) json_string_at(sub, p, out->system_slug, sizeof(out->system_slug));
      sub = find_key_in_span(system_obj, p, "name");
      if (sub) json_string_at(sub, p, out->system_name, sizeof(out->system_name));
    }
  }

  if (out->display_title[0] == '\0') {
    snprintf(out->display_title, sizeof(out->display_title), "%s", out->filename);
  }
  if (out->save_id[0] == '\0') {
    return -1;
  }

  return 0;
}

static int parse_array_of_save_objects(const char* body, const char* key,
                                       SgmSaveVersion* out_versions, int max_versions) {
  if (!body || !out_versions || max_versions <= 0) {
    return -1;
  }

  const char* arr_start = NULL;
  const char* arr_end = NULL;
  if (find_array_span(body, key, &arr_start, &arr_end) != 0) {
    return 0;
  }

  int count = 0;
  int in_string = 0;
  int escaped = 0;
  int depth = 0;
  const char* obj_start = NULL;

  for (const char* p = arr_start; p < arr_end; p++) {
    char ch = *p;

    if (in_string) {
      if (escaped) escaped = 0;
      else if (ch == '\\') escaped = 1;
      else if (ch == '"') in_string = 0;
      continue;
    }

    if (ch == '"') {
      in_string = 1;
      continue;
    }

    if (ch == '{') {
      if (depth == 0) {
        obj_start = p;
      }
      depth++;
    } else if (ch == '}') {
      depth--;
      if (depth == 0 && obj_start && count < max_versions) {
        if (parse_save_object(obj_start, p + 1, &out_versions[count]) == 0) {
          count++;
        }
        obj_start = NULL;
      }
    }
  }

  return count;
}

int sgm_json_health_ok(const char* body) {
  if (!body) {
    return 0;
  }
  return strstr(body, "\"ok\"") && strstr(body, "true");
}

int sgm_json_parse_saves(const char* body, SgmSaveVersion* out_versions, int max_versions) {
  return parse_array_of_save_objects(body, "saves", out_versions, max_versions);
}

int sgm_json_parse_versions(const char* body, SgmSaveVersion* out_versions, int max_versions) {
  return parse_array_of_save_objects(body, "versions", out_versions, max_versions);
}

static int ends_with_casefold(const char* text, const char* suffix) {
  if (!text || !suffix) {
    return 0;
  }

  size_t tlen = strlen(text);
  size_t slen = strlen(suffix);
  if (slen > tlen) {
    return 0;
  }

  const char* t = text + (tlen - slen);
  for (size_t i = 0; i < slen; i++) {
    if (tolower((unsigned char)t[i]) != tolower((unsigned char)suffix[i])) {
      return 0;
    }
  }
  return 1;
}

static int is_gamecube_version(const SgmSaveVersion* version) {
  if (!version) {
    return 0;
  }

  if (strstr(version->system_slug, "gamecube") || strstr(version->system_name, "GameCube")) {
    return 1;
  }

  return ends_with_casefold(version->filename, ".gci") ||
         ends_with_casefold(version->filename, ".gcs") ||
         ends_with_casefold(version->filename, ".sav") ||
         ends_with_casefold(version->filename, ".raw") ||
         ends_with_casefold(version->filename, ".gcp") ||
         ends_with_casefold(version->filename, ".mci");
}

int sgm_json_group_gamecube_games(const SgmSaveVersion* versions, int version_count,
                                  SgmBackendGame* out_games, int max_games) {
  if (!versions || !out_games || max_games <= 0) {
    return -1;
  }

  int count = 0;
  for (int i = 0; i < version_count; i++) {
    const SgmSaveVersion* v = &versions[i];
    if (!is_gamecube_version(v)) {
      continue;
    }

    char key[160];
    snprintf(key, sizeof(key), "%s|%s", v->display_title,
             v->system_slug[0] ? v->system_slug : "gamecube");

    int found = -1;
    for (int j = 0; j < count; j++) {
      if (strcmp(out_games[j].game_key, key) == 0) {
        found = j;
        break;
      }
    }

    if (found == -1) {
      if (count >= max_games) {
        continue;
      }
      SgmBackendGame* g = &out_games[count++];
      memset(g, 0, sizeof(*g));
      snprintf(g->game_key, sizeof(g->game_key), "%s", key);
      snprintf(g->display_title, sizeof(g->display_title), "%s", v->display_title);
      snprintf(g->system_slug, sizeof(g->system_slug), "%s",
               v->system_slug[0] ? v->system_slug : "gamecube");
      snprintf(g->latest_save_id, sizeof(g->latest_save_id), "%s", v->save_id);
      g->latest_version = v->version;
      g->version_count = 1;
      g->total_size = v->file_size;
    } else {
      SgmBackendGame* g = &out_games[found];
      g->version_count++;
      g->total_size += v->file_size;
      if (v->version >= g->latest_version) {
        g->latest_version = v->version;
        snprintf(g->latest_save_id, sizeof(g->latest_save_id), "%s", v->save_id);
      }
    }
  }

  return count;
}
