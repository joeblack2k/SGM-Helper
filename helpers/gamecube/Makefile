PLATFORM ?= gamecube
BASE_TARGET := sgm-gamecube-helper
TARGET := $(BASE_TARGET)
BUILD := build
SRC_DIR := src
INCLUDE_DIR := include

SOURCES := $(wildcard $(SRC_DIR)/*.c)
OBJECTS := $(patsubst $(SRC_DIR)/%.c,$(BUILD)/%.o,$(SOURCES))

CFLAGS_COMMON := -std=c11 -Wall -Wextra -I$(INCLUDE_DIR) -I$(SRC_DIR)

ifeq ($(PLATFORM),wii)
TARGET := sgm-wii-helper
PLATFORM_DEFINES := -DWII_TARGET -DHW_RVL
PLATFORM_CFLAGS := -mrvl -mcpu=750 -meabi -mhard-float
LIBOGC_LIBDIR := wii
else ifeq ($(PLATFORM),gamecube)
PLATFORM_DEFINES := -DGAMECUBE_TARGET -DHW_DOL -DGEKKO
PLATFORM_CFLAGS := -mcpu=750 -meabi -mhard-float
LIBOGC_LIBDIR := cube
else
$(error Unsupported PLATFORM '$(PLATFORM)'. Use PLATFORM=gamecube or PLATFORM=wii)
endif

.PHONY: all clean host test-host package-hbc

all: $(BUILD)/$(TARGET).dol

$(BUILD):
	@mkdir -p $(BUILD)

# Console build (devkitPro/libogc)
$(BUILD)/%.o: $(SRC_DIR)/%.c | $(BUILD)
	@if [ -z "$(DEVKITPRO)" ] || [ -z "$(DEVKITPPC)" ]; then \
		echo "DEVKITPRO/DEVKITPPC are required for console build"; \
		exit 1; \
	fi
	$(DEVKITPPC)/bin/powerpc-eabi-gcc $(CFLAGS_COMMON) $(PLATFORM_DEFINES) $(PLATFORM_CFLAGS) \
		-ffunction-sections -fdata-sections \
		-I$(DEVKITPRO)/libogc/include -c $< -o $@

$(BUILD)/$(TARGET).elf: $(OBJECTS)
	$(DEVKITPPC)/bin/powerpc-eabi-gcc $^ -o $@ \
		-L$(DEVKITPRO)/libogc/lib/$(LIBOGC_LIBDIR) -logc -lfat -lm \
		-Wl,-Map,$(BUILD)/$(TARGET).map -Wl,--gc-sections

$(BUILD)/$(TARGET).dol: $(BUILD)/$(TARGET).elf
	$(DEVKITPRO)/tools/bin/elf2dol $< $@
	@echo "Built $@ (PLATFORM=$(PLATFORM))"

package-hbc:
	@$(MAKE) PLATFORM=wii all
	@mkdir -p $(BUILD)/hbc/sgm-gamecube-helper
	@cp $(BUILD)/sgm-wii-helper.dol $(BUILD)/hbc/sgm-gamecube-helper/boot.dol
	@cp homebrew/sgm-gamecube-helper/meta.xml $(BUILD)/hbc/sgm-gamecube-helper/meta.xml
	@echo "Packaged Homebrew Launcher app in $(BUILD)/hbc/sgm-gamecube-helper"

# Host simulation build for local verification of non-hardware logic
host: $(BUILD)
	cc $(CFLAGS_COMMON) -DHOST_BUILD $(SOURCES) -o $(BUILD)/$(TARGET)-host
	@echo "Built $(BUILD)/$(TARGET)-host"

# Host unit tests
TEST_SOURCES := tests/host/test_main.c tests/host/test_secure_store.c tests/host/test_json_helpers.c $(SRC_DIR)/secure_store.c $(SRC_DIR)/json_helpers.c

test-host: $(BUILD)
	cc -std=c11 -Wall -Wextra -I$(INCLUDE_DIR) -I$(SRC_DIR) $(TEST_SOURCES) -o $(BUILD)/host-tests
	$(BUILD)/host-tests

clean:
	rm -rf $(BUILD)
