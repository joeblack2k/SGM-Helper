#include <stdio.h>
#include <string.h>

#include "../../src/secure_store.h"

int test_secure_store(void) {
  char compact[8] = {0};
  if (sgm_password_normalize("abc-123", compact, sizeof(compact)) != 0) {
    printf("normalize failed\n");
    return 1;
  }
  if (strcmp(compact, "ABC123") != 0) {
    printf("normalize mismatch: %s\n", compact);
    return 1;
  }

  char formatted[8] = {0};
  sgm_password_format(compact, formatted, sizeof(formatted));
  if (strcmp(formatted, "ABC-123") != 0) {
    printf("format mismatch: %s\n", formatted);
    return 1;
  }

  const char* path = "/tmp/sgm_gc_secure_store_test.bin";
  if (sgm_secure_store_save_password(path, "gc-fingerprint-1", compact) != 0) {
    printf("save failed\n");
    return 1;
  }

  char loaded[8] = {0};
  if (sgm_secure_store_load_password(path, "gc-fingerprint-1", loaded, sizeof(loaded)) != 0) {
    printf("load failed\n");
    return 1;
  }

  if (strcmp(loaded, "ABC123") != 0) {
    printf("loaded mismatch: %s\n", loaded);
    return 1;
  }

  char wrong[8] = {0};
  if (sgm_secure_store_load_password(path, "gc-fingerprint-2", wrong, sizeof(wrong)) == 0) {
    printf("load with wrong fingerprint unexpectedly succeeded\n");
    return 1;
  }

  return 0;
}
