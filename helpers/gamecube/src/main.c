#include <stdio.h>

#include "app.h"

int main(void) {
  int rc = sgm_gamecube_run();
  if (rc != 0) {
    printf("Fatal error: %d\n", rc);
  }
  return rc;
}
