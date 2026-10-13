# The guest kext: a Lilu plugin that x86-64 macOS guests load through
# OpenCore, beside Lilu, so their AppleParavirtGPU driver negotiates the
# binary version arm64 guests use (src/ReimsVgpu.cpp).
#
# reims-util puts ReimsVgpu.kext in its OpenCore images
# (../src/opencore/mod.rs), and builds anywhere, while the kext builds on a Mac
# alone. So the kext it takes is the one here, built and kept in the
# repository: rebuild it and commit it with any change to its sources.
# sources.sha256 records what it was built from, and reims-util's tests fail
# when that no longer matches.
#
#   make LILU=<Lilu.kext> KERNEL_SDK=<MacKernelSDK>
#               ReimsVgpu.kext and sources.sha256
#   make clean  remove build/, the objects
#
# LILU is a DEBUG build of Lilu 1.7.2, whose Contents/Resources holds the
# plugin headers and plugin_start.cpp; the image carries a RELEASE build of the
# same version. KERNEL_SDK is a checkout of acidanthera's MacKernelSDK. Builds
# on a Mac, with Xcode's clang and ld.

LILU       ?=
KERNEL_SDK ?=

BUILD   = build
NAME    = ReimsVgpu
VERSION = 0.1.0
KEXT    = $(NAME).kext
BINARY  = $(KEXT)/Contents/MacOS/$(NAME)

# The date Lilu's plugin_start.cpp stamps in the kext's version string, from
# __DATE__: fixed, so the same sources build the same bytes.
DATE = 2026-10-13
export SOURCE_DATE_EPOCH := $(shell date -j -u -f '%Y-%m-%d %H:%M:%S' '$(DATE) 00:00:00' +%s)

RESOURCES = $(LILU)/Contents/Resources

FLAGS = -arch x86_64 -mkernel -nostdlib -fno-builtin -fno-stack-protector \
	-mno-red-zone -Os -Wall -Wextra -Werror

CXXFLAGS = $(FLAGS) -std=c++17 -fapple-kext -fno-rtti -fno-exceptions \
	-DKERNEL -DKERNEL_PRIVATE -DLILU_KEXTPATCH_SUPPORT \
	-DPRODUCT_NAME=$(NAME) -DMODULE_VERSION=$(VERSION) \
	-isystem $(RESOURCES) -isystem $(KERNEL_SDK)/Headers

OBJECTS = $(BUILD)/obj/$(NAME).o $(BUILD)/obj/UserPatch.o $(BUILD)/obj/plugin_start.o

all: $(BINARY)

check-sdk:
	@test -f "$(RESOURCES)/Library/plugin_start.cpp" || \
		{ echo "LILU must name a DEBUG Lilu.kext"; exit 1; }
	@test -d "$(KERNEL_SDK)/Headers" || \
		{ echo "KERNEL_SDK must name a MacKernelSDK checkout"; exit 1; }

$(BUILD)/obj/%.o: src/%.cpp src/UserPatch.hpp Makefile | check-sdk
	@mkdir -p $(dir $@)
	xcrun clang++ $(CXXFLAGS) -c $< -o $@

$(BUILD)/obj/plugin_start.o: Makefile | check-sdk
	@mkdir -p $(dir $@)
	xcrun clang++ $(CXXFLAGS) -Wno-unused-parameter \
		-c $(RESOURCES)/Library/plugin_start.cpp -o $@

$(BINARY): $(OBJECTS) src/Info.plist
	@mkdir -p $(dir $@)
	xcrun ld -arch x86_64 -kext -undefined dynamic_lookup $(OBJECTS) -o $@
	sed 's/@VERSION@/$(VERSION)/g' src/Info.plist > $(KEXT)/Contents/Info.plist
	shasum -a 256 Makefile src/* > sources.sha256

clean:
	rm -rf $(BUILD)

.PHONY: all check-sdk clean
