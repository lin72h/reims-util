//
// UserPatch: patch bytes of an on-disk userspace binary, for every process
// that loads it, from the kext.
//
// Lilu's own user patcher is disabled on macOS 11 and later
// (kern_start.cpp: `isUserDisabled |= getKernelVersion() >= BigSur`), so a
// Lilu plugin cannot patch a userspace binary through `onProcLoad` there. The
// x86-64 AppleParavirtGPUMetal plugin hardcodes answers the arm64 plugin
// derives from the device, such as the GPU families its feature profile
// names, and those answers are the only place some fixes can live. This
// patches them.
//
// The mechanism is the one Lilu used before it gave up on userspace patching:
// route code signing's page validation, let the kernel validate the pristine
// page against its signature, and only then overwrite the bytes in that page.
// Since macOS 11 a page fault validates through `_cs_validate_page`, one page
// at a time; `_cs_validate_range`, which Lilu routed, does not see a binary's
// pages faulted in (measured on macOS 26.6.1's x86-64 kernel). The
// hash check has already passed on the unmodified bytes, so code signing does
// not notice; each time the page is re-read from the file and re-validated it
// is patched again. A patch applies only to a page whose whole content matches
// the target file's page, so no other binary is touched.
//
// Routing and loading are split: the validation hook goes in early
// (`onPatcherLoad`, before any userspace binary is validated), while the
// patterns are read from disk later (`onKextLoad`, once /System is mounted),
// because the plugin's pages must not be validated before the hook is live.
//

#ifndef ReimsVgpu_UserPatch_hpp
#define ReimsVgpu_UserPatch_hpp

#include <Headers/kern_patcher.hpp>

namespace UserPatch {

/// One find/replace in a file's `__TEXT,__text`. `find` must occur exactly
/// once there and within a single page, a page no other patch touches;
/// `replace` is the same length.
struct Patch {
    const char *path;
    const uint8_t *find;
    const uint8_t *replace;
    size_t length;
};

/// Routes `_cs_validate_page`. Call early (from `onPatcherLoad`), before any
/// target binary is validated. The hook does nothing until [`load`] runs.
void route(KernelPatcher &patcher);

/// Reads each patch's file and locates its pattern, arming the hook. Call once
/// the root filesystem is mounted (from an `onKextLoad` callback). A patch
/// whose file or pattern cannot be resolved is logged and skipped.
void load(const Patch *patches, size_t count);

/// The patcher's state: the routing result (0 routed, -1 not tried, else the
/// patcher's error), how many patches were located, how many times the
/// validation hook ran, and how many pages it patched.
void status(uint32_t &routed, uint32_t &located, uint32_t &calls, uint32_t &patched);

} // namespace UserPatch

#endif /* ReimsVgpu_UserPatch_hpp */
