// ReimsVgpu: a Lilu plugin for x86-64 macOS guests of reims-vgpu.
//
// The x86-64 AppleParavirtGPU.kext asks the device for binary version 6 and
// knows no later one: its setupVersion clamps to 6 and leaves every later
// feature off. arm64 guests ask for 245, the highest version the host knows
// (crates/reims-vgpu-core/src/guest/abi/features.rs in reims-vgpu). This
// plugin has the x86-64 driver ask for 245 too, and keeps the driver's own
// negotiation for a host that does not store it. What the x86-64 driver and
// its Metal plugin lack to run at 245, or hardcode where the arm64 ones ask
// the device, is patched here too: two-plane textures added as type 12, and
// the plugin's bytes (UserPatch).
//
// The offsets are AppleParavirtAccelerator's in macOS 26's x86-64 driver, read
// from its setupVersion: the device's registers at 0xe28, the negotiated
// version at 0xf48, and the 60 bytes of guest features after it, laid out as
// reims-vgpu-core's GuestFeatures.

#include <Headers/kern_api.hpp>
#include <Headers/plugin_start.hpp>

#include "UserPatch.hpp"

namespace {

// The Metal plugin (AppleParavirtGPUMetal) is a userspace binary the x86-64
// guest's frameworks load. The x86-64 build hardcodes answers the arm64 build
// derives from the device, so the fixes below live in its bytes, patched for
// every process through UserPatch (Lilu's own user patcher is off on Tahoe).
const char *metalPlugin =
    "/System/Library/Extensions/AppleParavirtGPUMetal.bundle/Contents/MacOS/AppleParavirtGPUMetal";

// -[AppleParavirtDevice featureProfile] returns 10000 (0x2710), whose families
// are Common1 and Common2. The arm64 plugin returns 10001 on a host GPU of no
// Apple family, adding Mac2 and Common3, which ANGLE's Metal backend (so a
// GPU-accelerated Chrome) needs. The method is `mov eax, 0x2710; pop rbp; ret`;
// 0x2710 becomes 0x2711. x86-64 guests run on hosts of no Apple family.
const uint8_t featureProfileFind[] {0x55, 0x48, 0x89, 0xe5, 0xb8, 0x10, 0x27, 0x00, 0x00, 0x5d, 0xc3};
const uint8_t featureProfileReplace[] {0x55, 0x48, 0x89, 0xe5, 0xb8, 0x11, 0x27, 0x00, 0x00, 0x5d, 0xc3};

// -[AppleParavirtTexture initTiledWithBuffer:descriptor:offset:bytesPerRow:] refuses a tiled,
// buffer-backed texture whose allocation size, every row counted at its full bytesPerRow stride,
// is larger than the buffer: it logs "Alloc size: %llu larger than provided buffer: %llu" and
// returns nil. SkyLight makes such textures for a window wider than two tiles, and the bytes the
// texture reads all fit, its last row ending at its width, not a full stride (arm64's plugin
// sidesteps this with private textures). The guard is `callq *r13; cmpq rax, r15; jbe ok` --
// continue when the allocation (r15) is within the buffer (rax); forcing the branch (`jbe` 0x76
// to `jmp` 0xeb) builds the texture always, and the host accepts it from its real footprint
// (crates/reims-vgpu-core/src/resources/texture/dimension.rs in reims-vgpu). On a page of its
// own, apart from featureProfile's.
const uint8_t tiledBoundsFind[] {0x4c, 0x89, 0xe7, 0x4c, 0x89, 0xf6, 0x41, 0xff, 0xd5,
                                 0x49, 0x39, 0xc7, 0x76, 0x4e};
const uint8_t tiledBoundsReplace[] {0x4c, 0x89, 0xe7, 0x4c, 0x89, 0xf6, 0x41, 0xff, 0xd5,
                                    0x49, 0x39, 0xc7, 0xeb, 0x4e};

UserPatch::Patch pluginPatches[] {
    {metalPlugin, featureProfileFind, featureProfileReplace, sizeof(featureProfileFind)},
    {metalPlugin, tiledBoundsFind, tiledBoundsReplace, sizeof(tiledBoundsFind)},
};

constexpr size_t RegistersOffset = 0xe28;
constexpr size_t VersionOffset = 0xf48;
constexpr size_t FeaturesOffset = 0xf4c;
// The binary version register.
constexpr size_t VersionRegister = 0x34;
constexpr uint32_t Version = 245;

// features_for_version(245).to_bytes(): every feature, 64 FIFOs and one byte
// of stencil for D32S8.
constexpr uint8_t Features[60] = {
    1, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // valid, unsupported, ...
    1, 0, 0, 0,                                     // buffer_from_io_surface, padding
    64, 0, 0, 0,                                    // max_fifo_count
    1, 0, 0, 0,                                     // s8_byte_count_for_d32s8
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // supports_shared_textures, ...
    1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, // ..., supports_multiple_displays
};

mach_vm_address_t originalSetupVersion;

// AppleParavirtAccelerator::setupVersion. A host that stores 245 gets the
// features of 245; any other answer goes back to the driver's negotiation,
// which writes its own version again.
void setupVersion(void *accelerator) {
    auto state = static_cast<uint8_t *>(accelerator);
    auto registers = *reinterpret_cast<volatile uint32_t **>(state + RegistersOffset);
    registers[VersionRegister / 4] = Version;
    if (registers[VersionRegister / 4] != Version) {
        SYSLOG("version", "the host did not store version %u", Version);
        FunctionCast(setupVersion, originalSetupVersion)(accelerator);
        return;
    }
    *reinterpret_cast<uint32_t *>(state + VersionOffset) = Version;
    memcpy(state + FeaturesOffset, Features, sizeof(Features));
    DBGLOG("version", "negotiated version %u", Version);
}

// The object types of a texture over a descriptor: one plane, and two. From
// version 31 the plugin lays out D32S8 and X32S8 as two planes, as the host
// does, but the x86-64 driver knows object types up to 7 and adds every such
// texture as type 2, which the host reads as one plane. arm64 guests add a
// two-plane texture as type 12.
constexpr uint32_t ObjectTexture = 2;
constexpr uint32_t ObjectTwoPlaneTexture = 12;
// A texture's record: 12 bytes of the object's own, then each plane's 0x18-byte
// dimension header (its low 14 bits the mip count) and 0x24 bytes per mip,
// then the descriptor command, whose type is 1 or 52.
constexpr size_t RecordHeader = 12;
constexpr size_t PlaneHeader = 0x18;
constexpr size_t MipRecord = 0x24;

uint16_t readU16(const uint8_t *record, size_t offset) {
    uint16_t value;
    memcpy(&value, record + offset, sizeof(value));
    return value;
}

bool isDescriptorCommand(const uint8_t *record, size_t length, size_t offset) {
    if (offset + sizeof(uint32_t) > length) return false;
    uint32_t type;
    memcpy(&type, record + offset, sizeof(type));
    return type == 1 || type == 52;
}

mach_vm_address_t originalAddObject;

// AppleParavirtResourceHeap::addObject. A type-2 texture whose record holds two
// planes of the same mip count before its descriptor command is added as type
// 12; every other object as the driver asked.
void *addObject(void *heap, uint32_t type, uint32_t index, uint32_t length, const void *data) {
    auto record = static_cast<const uint8_t *>(data);
    if (type == ObjectTexture && record && length >= RecordHeader + PlaneHeader) {
        uint16_t bits = readU16(record, RecordHeader);
        size_t plane = PlaneHeader + (bits & 0x3fff) * MipRecord;
        if (!isDescriptorCommand(record, length, RecordHeader + plane) &&
            isDescriptorCommand(record, length, RecordHeader + 2 * plane) &&
            (readU16(record, RecordHeader + plane) & 0x3fff) == (bits & 0x3fff)) {
            type = ObjectTwoPlaneTexture;
        }
    }
    return FunctionCast(addObject, originalAddObject)(heap, type, index, length, data);
}

const char *driverPaths[] {
    "/System/Library/Extensions/AppleParavirtGPU.kext/Contents/MacOS/AppleParavirtGPU",
};

KernelPatcher::KextInfo driver {
    "com.apple.driver.AppleParavirtGPU", driverPaths, arrsize(driverPaths), {true}, {},
    KernelPatcher::KextInfo::Unloaded,
};

void driverLoaded(void *, KernelPatcher &patcher, size_t index, mach_vm_address_t address,
                  size_t size) {
    if (index != driver.loadIndex) return;
    KernelPatcher::RouteRequest requests[] {
        {"__ZN24AppleParavirtAccelerator12setupVersionEv", setupVersion, originalSetupVersion},
        {"__ZN25AppleParavirtResourceHeap9addObjectE13APVObjectTypejjPKv", addObject,
         originalAddObject},
    };
    if (!patcher.routeMultiple(index, requests, arrsize(requests), address, size)) {
        SYSLOG("version", "cannot route the driver: %d", patcher.getError());
        patcher.clearError();
    }
    // The GPU kext loads after the root filesystem is mounted and before any
    // userspace Metal client loads the plugin, so read the plugin and arm the
    // patches here (`onPatcherLoad` is too early to read /System).
    UserPatch::load(pluginPatches, arrsize(pluginPatches));
}

// Publishes the user patcher's counters on the kext's IOService every few
// seconds, for `ioreg -c ReimsVgpu`: Lilu shows no plugin log on Tahoe.
thread_call_t statusCall;

void publishStatus(thread_call_param_t, thread_call_param_t) {
    uint32_t routed = 0, located = 0, calls = 0, patched = 0;
    UserPatch::status(routed, located, calls, patched);
    if (auto service = ADDPR(selfInstance)) {
        service->setProperty("UserPatchRouted", routed, 32);
        service->setProperty("UserPatchLocated", located, 32);
        service->setProperty("UserPatchValidateCalls", calls, 32);
        service->setProperty("UserPatchPagesPatched", patched, 32);
    }
    uint64_t deadline;
    clock_interval_to_deadline(5, kSecondScale, &deadline);
    thread_call_enter_delayed(statusCall, deadline);
}

void patcherLoaded(void *, KernelPatcher &patcher) {
    statusCall = thread_call_allocate(publishStatus, nullptr);
    if (statusCall) {
        publishStatus(nullptr, nullptr);
    }
    // Route code-signing validation early, before any binary is validated; the
    // hook stays inert until driverLoaded arms it.
    UserPatch::route(patcher);
}

void start() {
    lilu.onPatcherLoadForce(patcherLoaded);
    lilu.onKextLoadForce(&driver, 1, driverLoaded);
}

const char *bootArgsOff[] {"-reimsvgpuoff"};
const char *bootArgsDebug[] {"-reimsvgpudbg"};

} // namespace

PluginConfiguration ADDPR(config) {
    xStringify(PRODUCT_NAME),
    parseModuleVersion(xStringify(MODULE_VERSION)),
    LiluAPI::AllowNormal,
    bootArgsOff, arrsize(bootArgsOff),
    bootArgsDebug, arrsize(bootArgsDebug),
    nullptr, 0,
    KernelVersion::Tahoe, KernelVersion::Tahoe,
    start,
};

extern "C" kern_return_t ADDPR(kern_start)(kmod_info_t *, void *);
extern "C" kern_return_t ADDPR(kern_stop)(kmod_info_t *, void *);
KMOD_EXPLICIT_DECL(com.steelbrain.ReimsVgpu, xStringify(MODULE_VERSION), ADDPR(kern_start), ADDPR(kern_stop))
