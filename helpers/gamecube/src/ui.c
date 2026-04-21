#include "ui.h"

#include <stdio.h>
#include <string.h>

#include "../include/sgm_gc.h"
#include "platform.h"
#include "secure_store.h"

static void ui_render_title(const char* title, const char* subtitle) {
  sgm_platform_clear();
  sgm_platform_draw_line(0, title ? title : "Save Game Manager", 0);
  sgm_platform_draw_line(1, "", 0);
  if (subtitle && subtitle[0] != '\0') {
    sgm_platform_draw_line(2, subtitle, 0);
    sgm_platform_draw_line(3, "", 0);
  }
}

int sgm_ui_show_status(const char* title, const char* message, int wait_for_accept) {
  ui_render_title(title, "");
  sgm_platform_draw_line(4, message ? message : "", 0);
  sgm_platform_draw_line(6, wait_for_accept ? "Press A to continue" : "", 0);
  sgm_platform_present();

  if (!wait_for_accept) {
    sgm_platform_sleep_ms(900);
    return 0;
  }

  while (1) {
    SgmInput input;
    sgm_platform_poll_input(&input);
    if (input.accept || input.back || input.quit) {
      return 0;
    }
    sgm_platform_sleep_ms(20);
  }
}

int sgm_ui_select_list(const char* title, const char* subtitle, const char* const* items, int item_count) {
  if (!items || item_count <= 0) {
    sgm_ui_show_status(title, "No items", 1);
    return -1;
  }

  int selected = 0;
  while (1) {
    ui_render_title(title, subtitle);

    for (int i = 0; i < item_count; i++) {
      int row = 5 + i;
      sgm_platform_draw_line(row, items[i], i == selected);
      if (row > 24) {
        break;
      }
    }
    sgm_platform_draw_line(26, "A=Select  B=Back  X=Rescan  START=Quit", 0);
    sgm_platform_present();

    SgmInput input;
    sgm_platform_poll_input(&input);
    if (input.up) {
      selected = (selected + item_count - 1) % item_count;
    } else if (input.down) {
      selected = (selected + 1) % item_count;
    } else if (input.accept) {
      return selected;
    } else if (input.back || input.quit) {
      return -1;
    }
  }
}

int sgm_ui_confirm(const char* title, const char* question) {
  const char* options[2] = {"Yes", "No"};
  int choice = sgm_ui_select_list(title, question, options, 2);
  return choice == 0;
}

static int ui_prompt_with_controller(const char* title, const char* prompt,
                                     const char* allowed_chars,
                                     char* out, size_t out_size) {
  if (!allowed_chars || !out || out_size < 2) {
    return -1;
  }

  memset(out, 0, out_size);
  int cursor = 0;
  int char_index = 0;
  const int allowed_count = (int)strlen(allowed_chars);

  while (1) {
    char row_buf[196];
    ui_render_title(title, prompt);

    snprintf(row_buf, sizeof(row_buf), "Input: %s", out);
    sgm_platform_draw_line(5, row_buf, 0);

    snprintf(row_buf, sizeof(row_buf), "Cursor: %d / %d", cursor + 1, (int)out_size - 1);
    sgm_platform_draw_line(7, row_buf, 0);

    snprintf(row_buf, sizeof(row_buf), "Char: %c", allowed_chars[char_index]);
    sgm_platform_draw_line(8, row_buf, 0);

    sgm_platform_draw_line(10, "UP/DOWN: change char", 0);
    sgm_platform_draw_line(11, "LEFT/RIGHT: move cursor", 0);
    sgm_platform_draw_line(12, "A: accept  B: cancel  X: clear", 0);
    sgm_platform_present();

    SgmInput input;
    sgm_platform_poll_input(&input);

    if (input.up) {
      char_index = (char_index + 1) % allowed_count;
      out[cursor] = allowed_chars[char_index];
    } else if (input.down) {
      char_index = (char_index + allowed_count - 1) % allowed_count;
      out[cursor] = allowed_chars[char_index];
    } else if (input.right) {
      if (cursor < (int)out_size - 2) {
        cursor++;
      }
      if (out[cursor] == '\0') {
        out[cursor] = allowed_chars[char_index];
      }
    } else if (input.left) {
      if (cursor > 0) {
        cursor--;
      }
      if (out[cursor] == '\0') {
        out[cursor] = allowed_chars[char_index];
      }
    } else if (input.rescan) {
      memset(out, 0, out_size);
      cursor = 0;
      char_index = 0;
    } else if (input.back || input.quit) {
      return -1;
    } else if (input.accept) {
      size_t len = strlen(out);
      while (len > 0 && out[len - 1] == ' ') {
        out[len - 1] = '\0';
        len--;
      }
      if (len > 0) {
        return 0;
      }
    }

    sgm_platform_sleep_ms(20);
  }
}

int sgm_ui_prompt_password(char* out_compact, size_t out_size) {
  if (!out_compact || out_size < SGM_PASSWORD_COMPACT_LEN + 1) {
    return -1;
  }

  char raw[32] = {0};
  if (sgm_platform_prompt_line("Enter device password", "Use 6 chars (ABC123 or ABC-123)", raw,
                               sizeof(raw)) == 0) {
    if (sgm_password_normalize(raw, out_compact, out_size) == 0) {
      return 0;
    }
    sgm_ui_show_status("Invalid password", "Expected 6 chars, A-Z0-9", 1);
  }

  if (ui_prompt_with_controller("Enter device password",
                                "Use UP/DOWN for chars, A when done",
                                "ABCDEFGHJKLMNPQRSTUVWXYZ23456789-",
                                raw, sizeof(raw)) != 0) {
    return -1;
  }

  if (sgm_password_normalize(raw, out_compact, out_size) != 0) {
    sgm_ui_show_status("Invalid password", "Expected 6 chars, A-Z0-9", 1);
    return -1;
  }
  return 0;
}

int sgm_ui_prompt_manual_ip(char* out_ip, size_t out_size) {
  if (!out_ip || out_size < 8) {
    return -1;
  }

  if (sgm_platform_prompt_line("Manual server IP", "Enter IPv4 address", out_ip, out_size) == 0) {
    if (strchr(out_ip, '.') != NULL) {
      return 0;
    }
  }

  if (ui_prompt_with_controller("Manual server IP", "Enter IPv4 (example 192.168.1.10)",
                                "0123456789.", out_ip, out_size) != 0) {
    return -1;
  }
  return strchr(out_ip, '.') ? 0 : -1;
}
