#ifndef SGM_GC_PLATFORM_H
#define SGM_GC_PLATFORM_H

#include <stddef.h>

#include "../include/sgm_gc.h"

int sgm_platform_init(void);
void sgm_platform_shutdown(void);
void sgm_platform_sleep_ms(int ms);
void sgm_platform_clear(void);
void sgm_platform_present(void);
void sgm_platform_draw_line(int row, const char* text, int selected);
int sgm_platform_poll_input(SgmInput* input);
int sgm_platform_prompt_line(const char* title, const char* prompt, char* out, size_t out_size);
int sgm_platform_get_local_ip(char* out, size_t out_size);
int sgm_platform_get_fingerprint(char* out, size_t out_size);

#endif
