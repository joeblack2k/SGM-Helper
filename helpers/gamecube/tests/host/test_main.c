#include <stdio.h>

int test_secure_store(void);
int test_json_helpers(void);

int main(void) {
  if (test_secure_store() != 0) {
    return 1;
  }
  if (test_json_helpers() != 0) {
    return 1;
  }

  printf("host tests passed\n");
  return 0;
}
