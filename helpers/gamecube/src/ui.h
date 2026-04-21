#ifndef SGM_GC_UI_H
#define SGM_GC_UI_H

#include <stddef.h>

int sgm_ui_show_status(const char* title, const char* message, int wait_for_accept);
int sgm_ui_select_list(const char* title, const char* subtitle, const char* const* items, int item_count);
int sgm_ui_confirm(const char* title, const char* question);
int sgm_ui_prompt_password(char* out_compact, size_t out_size);
int sgm_ui_prompt_manual_ip(char* out_ip, size_t out_size);

#endif
