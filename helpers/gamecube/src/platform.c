#include "platform.h"

#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

#if defined(GAMECUBE_TARGET) || defined(WII_TARGET)
#include <fat.h>
#include <gccore.h>
#include <network.h>
#include <ogc/pad.h>
#ifdef WII_TARGET
#include <wiiuse/wpad.h>
#endif

static void* g_xfb = NULL;
static GXRModeObj* g_rmode = NULL;

int sgm_platform_init(void) {
  VIDEO_Init();
  PAD_Init();
#ifdef WII_TARGET
  WPAD_Init();
#endif

  g_rmode = VIDEO_GetPreferredMode(NULL);
  g_xfb = MEM_K0_TO_K1(SYS_AllocateFramebuffer(g_rmode));
  if (!g_xfb || !g_rmode) {
    return -1;
  }

  console_init(g_xfb, 20, 20, g_rmode->fbWidth, g_rmode->xfbHeight,
               g_rmode->fbWidth * VI_DISPLAY_PIX_SZ);

  VIDEO_Configure(g_rmode);
  VIDEO_SetNextFramebuffer(g_xfb);
  VIDEO_SetBlack(FALSE);
  VIDEO_Flush();
  VIDEO_WaitVSync();
  if (g_rmode->viTVMode & VI_NON_INTERLACE) {
    VIDEO_WaitVSync();
  }

  fatInitDefault();
  return 0;
}

void sgm_platform_shutdown(void) {
#ifdef WII_TARGET
  WPAD_Shutdown();
#endif
}

void sgm_platform_sleep_ms(int ms) {
  if (ms <= 0) {
    return;
  }
  usleep((useconds_t)ms * 1000);
}

void sgm_platform_clear(void) {
  printf("\x1b[2J\x1b[H");
}

void sgm_platform_present(void) {
  VIDEO_Flush();
  VIDEO_WaitVSync();
}

void sgm_platform_draw_line(int row, const char* text, int selected) {
  const char* marker = selected ? ">" : " ";
  printf("\x1b[%d;1H%s %s\n", row + 1, marker, text ? text : "");
}

int sgm_platform_poll_input(SgmInput* input) {
  if (!input) {
    return -1;
  }
  memset(input, 0, sizeof(*input));

  PAD_ScanPads();
  u32 gc_buttons = PAD_ButtonsDown(0);

#ifdef WII_TARGET
  WPAD_ScanPads();
  u32 wii_buttons = WPAD_ButtonsDown(0);
  u32 wii_classic = WPAD_ButtonsDown(0);

  input->up = ((gc_buttons & PAD_BUTTON_UP) != 0) ||
              ((wii_buttons & WPAD_BUTTON_UP) != 0) ||
              ((wii_classic & WPAD_CLASSIC_BUTTON_UP) != 0);
  input->down = ((gc_buttons & PAD_BUTTON_DOWN) != 0) ||
                ((wii_buttons & WPAD_BUTTON_DOWN) != 0) ||
                ((wii_classic & WPAD_CLASSIC_BUTTON_DOWN) != 0);
  input->left = ((gc_buttons & PAD_BUTTON_LEFT) != 0) ||
                ((wii_buttons & WPAD_BUTTON_LEFT) != 0) ||
                ((wii_classic & WPAD_CLASSIC_BUTTON_LEFT) != 0);
  input->right = ((gc_buttons & PAD_BUTTON_RIGHT) != 0) ||
                 ((wii_buttons & WPAD_BUTTON_RIGHT) != 0) ||
                 ((wii_classic & WPAD_CLASSIC_BUTTON_RIGHT) != 0);
  input->accept = ((gc_buttons & PAD_BUTTON_A) != 0) ||
                  ((wii_buttons & WPAD_BUTTON_A) != 0) ||
                  ((wii_classic & WPAD_CLASSIC_BUTTON_A) != 0);
  input->back = ((gc_buttons & PAD_BUTTON_B) != 0) ||
                ((wii_buttons & WPAD_BUTTON_B) != 0) ||
                ((wii_buttons & WPAD_BUTTON_2) != 0) ||
                ((wii_classic & WPAD_CLASSIC_BUTTON_B) != 0);
  input->rescan = ((gc_buttons & PAD_BUTTON_X) != 0) ||
                  ((wii_buttons & WPAD_BUTTON_1) != 0) ||
                  ((wii_classic & WPAD_CLASSIC_BUTTON_X) != 0);
  input->quit = ((gc_buttons & PAD_BUTTON_START) != 0) ||
                ((wii_buttons & WPAD_BUTTON_HOME) != 0);
#else
  input->up = (gc_buttons & PAD_BUTTON_UP) != 0;
  input->down = (gc_buttons & PAD_BUTTON_DOWN) != 0;
  input->left = (gc_buttons & PAD_BUTTON_LEFT) != 0;
  input->right = (gc_buttons & PAD_BUTTON_RIGHT) != 0;
  input->accept = (gc_buttons & PAD_BUTTON_A) != 0;
  input->back = (gc_buttons & PAD_BUTTON_B) != 0;
  input->rescan = (gc_buttons & PAD_BUTTON_X) != 0;
  input->quit = (gc_buttons & PAD_BUTTON_START) != 0;
#endif
  return 0;
}

int sgm_platform_prompt_line(const char* title, const char* prompt, char* out, size_t out_size) {
  (void)title;
  (void)prompt;
  (void)out;
  (void)out_size;
  return -1;
}

int sgm_platform_get_local_ip(char* out, size_t out_size) {
  if (!out || out_size < 8) {
    return -1;
  }

  char local_ip[32] = {0};
  char gateway[32] = {0};
  char mask[32] = {0};

  if (if_config(local_ip, mask, gateway, TRUE, 20) < 0) {
    return -1;
  }

  snprintf(out, out_size, "%s", local_ip);
  return 0;
}

int sgm_platform_get_fingerprint(char* out, size_t out_size) {
  if (!out || out_size < 16) {
    return -1;
  }

  char ip[32] = {0};
  if (sgm_platform_get_local_ip(ip, sizeof(ip)) != 0) {
    snprintf(ip, sizeof(ip), "0.0.0.0");
  }

  u32 console_type = SYS_GetConsoleType();
#ifdef WII_TARGET
  snprintf(out, out_size, "wii-%08lx-%s", (unsigned long)console_type, ip);
#else
  snprintf(out, out_size, "gc-%08lx-%s", (unsigned long)console_type, ip);
#endif
  return 0;
}

#else

#include <arpa/inet.h>
#include <ifaddrs.h>
#include <netinet/in.h>
#include <sys/select.h>
#include <sys/time.h>
#include <sys/types.h>
#include <unistd.h>

int sgm_platform_init(void) { return 0; }

void sgm_platform_shutdown(void) {}

void sgm_platform_sleep_ms(int ms) {
  if (ms <= 0) {
    return;
  }
  usleep((useconds_t)ms * 1000);
}

void sgm_platform_clear(void) { printf("\x1b[2J\x1b[H"); }

void sgm_platform_present(void) { fflush(stdout); }

void sgm_platform_draw_line(int row, const char* text, int selected) {
  const char* marker = selected ? ">" : " ";
  printf("\x1b[%d;1H%s %s\n", row + 1, marker, text ? text : "");
}

static void read_host_input(SgmInput* input) {
  printf("\n[w/s/a/d=nav, e=accept, q=back, x=rescan, p=quit] > ");
  fflush(stdout);
  int ch = getchar();
  while (ch == '\n' || ch == '\r') {
    ch = getchar();
  }

  if (ch == 'w') input->up = 1;
  if (ch == 's') input->down = 1;
  if (ch == 'a') input->left = 1;
  if (ch == 'd') input->right = 1;
  if (ch == 'e') input->accept = 1;
  if (ch == 'q') input->back = 1;
  if (ch == 'x') input->rescan = 1;
  if (ch == 'p') input->quit = 1;

  int c;
  while ((c = getchar()) != '\n' && c != EOF) {
  }
}

int sgm_platform_poll_input(SgmInput* input) {
  if (!input) {
    return -1;
  }
  memset(input, 0, sizeof(*input));
  read_host_input(input);
  return 0;
}

int sgm_platform_prompt_line(const char* title, const char* prompt, char* out, size_t out_size) {
  if (!out || out_size == 0) {
    return -1;
  }
  printf("\n%s\n%s\n> ", title ? title : "Input", prompt ? prompt : "");
  fflush(stdout);

  if (!fgets(out, (int)out_size, stdin)) {
    return -1;
  }
  size_t len = strlen(out);
  while (len > 0 && (out[len - 1] == '\n' || out[len - 1] == '\r')) {
    out[len - 1] = '\0';
    len--;
  }
  return 0;
}

int sgm_platform_get_local_ip(char* out, size_t out_size) {
  if (!out || out_size < 8) {
    return -1;
  }

  const char* forced = getenv("SGM_LOCAL_IP");
  if (forced && forced[0] != '\0') {
    snprintf(out, out_size, "%s", forced);
    return 0;
  }

  struct ifaddrs* ifaddr = NULL;
  if (getifaddrs(&ifaddr) == -1) {
    snprintf(out, out_size, "192.168.1.2");
    return 0;
  }

  for (struct ifaddrs* it = ifaddr; it; it = it->ifa_next) {
    if (!it->ifa_addr || it->ifa_addr->sa_family != AF_INET) {
      continue;
    }
    if (strncmp(it->ifa_name, "lo", 2) == 0) {
      continue;
    }

    const struct sockaddr_in* sin = (const struct sockaddr_in*)it->ifa_addr;
    const char* ip = inet_ntoa(sin->sin_addr);
    if (ip && strcmp(ip, "127.0.0.1") != 0) {
      snprintf(out, out_size, "%s", ip);
      freeifaddrs(ifaddr);
      return 0;
    }
  }

  freeifaddrs(ifaddr);
  snprintf(out, out_size, "192.168.1.2");
  return 0;
}

int sgm_platform_get_fingerprint(char* out, size_t out_size) {
  if (!out || out_size < 12) {
    return -1;
  }

  char hostname[128] = {0};
  if (gethostname(hostname, sizeof(hostname) - 1) != 0) {
    snprintf(hostname, sizeof(hostname), "host");
  }
  snprintf(out, out_size, "host-%s", hostname);
  return 0;
}

#endif
