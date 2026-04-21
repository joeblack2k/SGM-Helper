#include "secure_store.h"

#include <ctype.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#define STORE_MAGIC "SGM1"

typedef struct {
  char magic[4];
  uint32_t nonce;
  uint8_t length;
  uint8_t reserved[3];
  uint8_t ciphertext[16];
  uint32_t checksum;
} PasswordBlob;

static uint32_t fnv1a_u32(const uint8_t* data, size_t len, uint32_t seed) {
  uint32_t hash = seed;
  for (size_t i = 0; i < len; i++) {
    hash ^= data[i];
    hash *= 16777619u;
  }
  return hash;
}

static void derive_key(const char* fingerprint, uint32_t key[4]) {
  const char* src = fingerprint ? fingerprint : "gamecube-sgm";
  uint32_t h = 2166136261u;
  size_t len = strlen(src);

  for (int i = 0; i < 4; i++) {
    h = fnv1a_u32((const uint8_t*)src, len, h ^ (uint32_t)(0x9E3779B9u * (uint32_t)(i + 1)));
    key[i] = h ^ (0xA5A5A5A5u + (uint32_t)i * 0x1f1f1f1fu);
  }
}

static void xtea_encrypt_block(uint32_t v[2], const uint32_t key[4]) {
  uint32_t sum = 0;
  const uint32_t delta = 0x9E3779B9u;

  for (int i = 0; i < 32; i++) {
    v[0] += (((v[1] << 4) ^ (v[1] >> 5)) + v[1]) ^ (sum + key[sum & 3]);
    sum += delta;
    v[1] += (((v[0] << 4) ^ (v[0] >> 5)) + v[0]) ^ (sum + key[(sum >> 11) & 3]);
  }
}

static void stream_xor(uint8_t* data, size_t len, uint32_t nonce, const uint32_t key[4]) {
  uint32_t counter = 0;
  for (size_t offset = 0; offset < len; offset += 8) {
    uint32_t block[2] = {nonce, counter++};
    xtea_encrypt_block(block, key);

    uint8_t stream[8];
    memcpy(stream, &block[0], 4);
    memcpy(stream + 4, &block[1], 4);

    size_t chunk = (len - offset > 8) ? 8 : (len - offset);
    for (size_t i = 0; i < chunk; i++) {
      data[offset + i] ^= stream[i];
    }
  }
}

int sgm_password_normalize(const char* raw, char* out_compact, size_t out_size) {
  if (!raw || !out_compact || out_size < 7) {
    return -1;
  }

  char compact[8] = {0};
  size_t write = 0;
  for (size_t i = 0; raw[i] != '\0'; i++) {
    char ch = raw[i];
    if (ch == '-' || ch == ' ' || ch == '\t' || ch == '\n' || ch == '\r') {
      continue;
    }
    ch = (char)toupper((unsigned char)ch);
    if (!((ch >= 'A' && ch <= 'Z') || (ch >= '0' && ch <= '9'))) {
      return -1;
    }
    if (write >= 6) {
      return -1;
    }
    compact[write++] = ch;
  }

  if (write != 6) {
    return -1;
  }

  snprintf(out_compact, out_size, "%s", compact);
  return 0;
}

void sgm_password_format(const char* compact, char* out_formatted, size_t out_size) {
  if (!out_formatted || out_size < 8) {
    return;
  }
  out_formatted[0] = '\0';

  if (!compact) {
    return;
  }

  char normalized[8] = {0};
  if (sgm_password_normalize(compact, normalized, sizeof(normalized)) != 0) {
    return;
  }

  snprintf(out_formatted, out_size, "%.3s-%.3s", normalized, normalized + 3);
}

int sgm_secure_store_save_password(const char* path, const char* fingerprint,
                                   const char* compact_password) {
  if (!path || !compact_password) {
    return -1;
  }

  char normalized[8] = {0};
  if (sgm_password_normalize(compact_password, normalized, sizeof(normalized)) != 0) {
    return -1;
  }

  uint32_t key[4] = {0};
  derive_key(fingerprint, key);

  PasswordBlob blob;
  memset(&blob, 0, sizeof(blob));
  memcpy(blob.magic, STORE_MAGIC, 4);
  blob.nonce = (uint32_t)time(NULL) ^ 0xC0FFEE12u;
  blob.length = 6;

  memcpy(blob.ciphertext, normalized, 6);
  stream_xor(blob.ciphertext, blob.length, blob.nonce, key);

  blob.checksum = fnv1a_u32((const uint8_t*)normalized, 6, 2166136261u);
  if (fingerprint) {
    blob.checksum = fnv1a_u32((const uint8_t*)fingerprint, strlen(fingerprint), blob.checksum);
  }

  FILE* fp = fopen(path, "wb");
  if (!fp) {
    return -1;
  }
  size_t written = fwrite(&blob, 1, sizeof(blob), fp);
  fclose(fp);
  return written == sizeof(blob) ? 0 : -1;
}

int sgm_secure_store_load_password(const char* path, const char* fingerprint,
                                   char* out_compact_password, size_t out_size) {
  if (!path || !out_compact_password || out_size < 7) {
    return -1;
  }

  FILE* fp = fopen(path, "rb");
  if (!fp) {
    return -1;
  }

  PasswordBlob blob;
  size_t read = fread(&blob, 1, sizeof(blob), fp);
  fclose(fp);
  if (read != sizeof(blob)) {
    return -1;
  }

  if (memcmp(blob.magic, STORE_MAGIC, 4) != 0 || blob.length != 6) {
    return -1;
  }

  uint32_t key[4] = {0};
  derive_key(fingerprint, key);

  uint8_t plaintext[16] = {0};
  memcpy(plaintext, blob.ciphertext, blob.length);
  stream_xor(plaintext, blob.length, blob.nonce, key);

  uint32_t checksum = fnv1a_u32(plaintext, blob.length, 2166136261u);
  if (fingerprint) {
    checksum = fnv1a_u32((const uint8_t*)fingerprint, strlen(fingerprint), checksum);
  }

  if (checksum != blob.checksum) {
    return -1;
  }

  plaintext[6] = '\0';
  if (sgm_password_normalize((const char*)plaintext, out_compact_password, out_size) != 0) {
    return -1;
  }

  return 0;
}
