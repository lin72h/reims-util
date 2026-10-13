#include "UserPatch.hpp"

#include <Headers/kern_file.hpp>
#include <Headers/kern_mach.hpp>
#include <Headers/kern_util.hpp>

#include <mach/memory_object_types.h>

namespace UserPatch {

// The x86-64 guest's page is 4 KiB; the pristine page store is sized for it.
static constexpr size_t PageSize = 4096;
static constexpr size_t MaxPagePatches = 8;
static constexpr size_t ReplaceMax = 64;
// The fast reject reads this many bytes of the pattern; patterns are longer.
static constexpr size_t CheckLength = 8;

// A located patch: the file page that holds the pattern, kept whole so a page
// is patched only when its entire content still matches the file.
struct PagePatch {
    const char *path;
    size_t pageBase;
    uint8_t pristine[PageSize];
    uint32_t pageLength;
    uint32_t patchOffset;
    uint8_t replace[ReplaceMax];
    uint32_t replaceLength;
};

static PagePatch pagePatches[MaxPagePatches];
static size_t pagePatchCount;
static bool armed;
static mach_vm_address_t orgCsValidatePage;

// Counters published on the IORegistry by [`status`], readable with `ioreg`
// as any user: Lilu writes its debug dump only while its own user patcher is
// active, which it never is on Tahoe, so SYSLOG output is not visible there.
static volatile uint32_t validateCalls;
static volatile uint32_t pagesPatched;
static int routeError = -1;

static const uint8_t *search(const uint8_t *hay, size_t n, const uint8_t *needle, size_t m) {
    if (m == 0 || n < m) {
        return nullptr;
    }
    for (size_t i = 0; i + m <= n; i++) {
        if (memcmp(hay + i, needle, m) == 0) {
            return hay + i;
        }
    }
    return nullptr;
}

// Patch a just-validated page whose whole content still matches a target file
// page. Runs after the kernel has validated the pristine bytes.
static void apply(const uint8_t *page) {
    for (size_t i = 0; i < pagePatchCount; i++) {
        auto &pp = pagePatches[i];
        if (memcmp(page + pp.patchOffset, pp.pristine + pp.patchOffset, CheckLength) != 0) {
            continue;
        }
        if (memcmp(page, pp.pristine, pp.pageLength) != 0) {
            continue;
        }
        if (MachInfo::setKernelWriting(true, KernelPatcher::kernelWriteLock) == KERN_SUCCESS) {
            memcpy(const_cast<uint8_t *>(page) + pp.patchOffset, pp.replace, pp.replaceLength);
            MachInfo::setKernelWriting(false, KernelPatcher::kernelWriteLock);
            pagesPatched = pagesPatched + 1;
        }
    }
}

// The page fault path (vm_page_validate_cs_mapped_slow) validates one page at
// a time through cs_validate_page, which reports through its out-parameters:
// `validated` and `tainted` are nonzero masks of the page's 4 KiB chunks.
static void csValidatePage(vnode_t vp, memory_object_t pager, memory_object_offset_t page_offset,
                           const void *data, int *validated, int *tainted, int *nx) {
    FunctionCast(csValidatePage, orgCsValidatePage)(vp, pager, page_offset, data, validated,
                                                    tainted, nx);
    validateCalls = validateCalls + 1;
    if (armed && *validated && !*tainted) {
        apply(static_cast<const uint8_t *>(data));
    }
}

void route(KernelPatcher &patcher) {
    KernelPatcher::RouteRequest request {"_cs_validate_page", csValidatePage, orgCsValidatePage};
    if (!patcher.routeMultipleLong(KernelPatcher::KernelID, &request, 1)) {
        routeError = static_cast<int>(patcher.getError());
        SYSLOG("userpatch", "cannot route _cs_validate_page: %d", routeError);
        patcher.clearError();
    } else {
        routeError = 0;
    }
}

void status(uint32_t &routed, uint32_t &located, uint32_t &calls, uint32_t &patched) {
    routed = static_cast<uint32_t>(routeError);
    located = static_cast<uint32_t>(pagePatchCount);
    calls = validateCalls;
    patched = pagesPatched;
}

static bool samePage(const char *path, size_t pageBase) {
    for (size_t i = 0; i < pagePatchCount; i++) {
        if (pagePatches[i].pageBase == pageBase && strcmp(pagePatches[i].path, path) == 0) {
            return true;
        }
    }
    return false;
}

static bool locate(const Patch &patch) {
    if (patch.length < CheckLength || patch.length > ReplaceMax) {
        SYSLOG("userpatch", "patch of %s has unusable length %zu", patch.path, patch.length);
        return false;
    }
    size_t fileSize = 0;
    auto buffer = FileIO::readFileToBuffer(patch.path, fileSize);
    if (!buffer) {
        SYSLOG("userpatch", "cannot read %s", patch.path);
        return false;
    }
    bool located = false;
    vm_address_t segment = 0, section = 0;
    void *text = nullptr;
    size_t textSize = 0;
    MachInfo::findSectionBounds(buffer, fileSize, segment, section, text, textSize,
                               "__TEXT", "__text", CPU_TYPE_X86_64);
    if (text && textSize) {
        auto textEnd = static_cast<uint8_t *>(text) + textSize;
        auto hit = search(static_cast<uint8_t *>(text), textSize, patch.find, patch.length);
        auto after = hit ? search(hit + 1, static_cast<size_t>(textEnd - (hit + 1)),
                                  patch.find, patch.length)
                         : nullptr;
        if (!hit) {
            SYSLOG("userpatch", "pattern not found in %s", patch.path);
        } else if (after) {
            SYSLOG("userpatch", "pattern not unique in %s", patch.path);
        } else {
            size_t fileOffset = static_cast<size_t>(hit - buffer);
            size_t pageBase = fileOffset & ~(PageSize - 1);
            size_t inPage = fileOffset - pageBase;
            if (inPage + patch.length > PageSize) {
                SYSLOG("userpatch", "pattern spans a page in %s", patch.path);
            } else if (pagePatchCount >= MaxPagePatches) {
                SYSLOG("userpatch", "too many page patches");
            } else if (samePage(patch.path, pageBase)) {
                // A page is patched only while it matches its file page whole,
                // so a second patch of a patched page would never apply.
                SYSLOG("userpatch", "a second patch of one page in %s", patch.path);
            } else {
                auto &pp = pagePatches[pagePatchCount];
                pp.path = patch.path;
                pp.pageBase = pageBase;
                size_t pageLength = fileSize - pageBase;
                if (pageLength > PageSize) {
                    pageLength = PageSize;
                }
                memset(pp.pristine, 0, PageSize);
                memcpy(pp.pristine, buffer + pageBase, pageLength);
                pp.pageLength = static_cast<uint32_t>(pageLength);
                pp.patchOffset = static_cast<uint32_t>(inPage);
                memcpy(pp.replace, patch.replace, patch.length);
                pp.replaceLength = static_cast<uint32_t>(patch.length);
                pagePatchCount++;
                located = true;
                SYSLOG("userpatch", "located %s at file offset %zu", patch.path, fileOffset);
            }
        }
    } else {
        SYSLOG("userpatch", "no __text in %s", patch.path);
    }
    Buffer::deleter(buffer);
    return located;
}

void load(const Patch *patches, size_t count) {
    if (PAGE_SIZE != PageSize) {
        SYSLOG("userpatch", "unexpected page size %u", static_cast<unsigned>(PAGE_SIZE));
        return;
    }
    for (size_t i = 0; i < count; i++) {
        locate(patches[i]);
    }
    armed = pagePatchCount != 0;
    SYSLOG("userpatch", "armed %zu page patch(es)", pagePatchCount);
}

} // namespace UserPatch
