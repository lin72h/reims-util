// Run in the guest: what Metal's device answers, and whether the objects the
// answers promise can be made. Prints one `name value` line each.
//
//   xcrun clang -arch x86_64 -fobjc-arc -framework Metal -framework Foundation \
//     features.m -o features
#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

static void line(const char *name, long long value) { printf("%s %lld\n", name, value); }

int main(void) {
    @autoreleasepool {
        id<MTLDevice> device = MTLCreateSystemDefaultDevice();
        if (!device) { puts("device none"); return 1; }
        printf("device %s\n", device.name.UTF8String);
        printf("architecture %s\n", device.architecture.name.UTF8String ?: "nil");
        const struct { const char *name; MTLGPUFamily family; } families[] = {
            {"family.apple1", MTLGPUFamilyApple1}, {"family.apple2", MTLGPUFamilyApple2},
            {"family.apple3", MTLGPUFamilyApple3}, {"family.apple4", MTLGPUFamilyApple4},
            {"family.apple5", MTLGPUFamilyApple5}, {"family.apple6", MTLGPUFamilyApple6},
            {"family.apple7", MTLGPUFamilyApple7}, {"family.apple8", MTLGPUFamilyApple8},
            {"family.apple9", MTLGPUFamilyApple9}, {"family.mac2", MTLGPUFamilyMac2},
            {"family.common1", MTLGPUFamilyCommon1}, {"family.common2", MTLGPUFamilyCommon2},
            {"family.common3", MTLGPUFamilyCommon3}, {"family.metal3", MTLGPUFamilyMetal3},
            {"family.metal4", MTLGPUFamilyMetal4},
        };
        for (size_t i = 0; i < sizeof(families) / sizeof(*families); ++i)
            line(families[i].name, [device supportsFamily:families[i].family]);
        line("argumentBuffersSupport", device.argumentBuffersSupport);
        line("hasUnifiedMemory", device.hasUnifiedMemory);
        line("maxBufferLength", (long long)device.maxBufferLength);
        line("maxThreadgroupMemoryLength", (long long)device.maxThreadgroupMemoryLength);
        line("maxThreadsPerThreadgroup.width", (long long)device.maxThreadsPerThreadgroup.width);
        line("recommendedMaxWorkingSetSize", (long long)device.recommendedMaxWorkingSetSize);
        line("supportsRaytracing", device.supportsRaytracing);
        line("supportsFunctionPointers", device.supportsFunctionPointers);
        line("supportsDynamicLibraries", device.supportsDynamicLibraries);
        line("supportsRenderDynamicLibraries", device.supportsRenderDynamicLibraries);
        line("supports32BitFloatFiltering", device.supports32BitFloatFiltering);
        line("supports32BitMSAA", device.supports32BitMSAA);
        line("supportsBCTextureCompression", device.supportsBCTextureCompression);
        line("supportsPullModelInterpolation", device.supportsPullModelInterpolation);
        line("supportsShaderBarycentricCoordinates", device.supportsShaderBarycentricCoordinates);
        line("supportsQueryTextureLOD", device.supportsQueryTextureLOD);
        line("supportsVertexAmplificationCount2", [device supportsVertexAmplificationCount:2]);
        line("supportsRasterizationRateMap1", [device supportsRasterizationRateMapWithLayerCount:1]);
        line("programmableSamplePositions", device.programmableSamplePositionsSupported);
        line("rasterOrderGroups", device.rasterOrderGroupsSupported);
        line("depth24Stencil8", device.depth24Stencil8PixelFormatSupported);
        line("readWriteTextureSupport", device.readWriteTextureSupport);
        line("sparseTileSizeInBytes", (long long)device.sparseTileSizeInBytes);
        line("binaryArchives", [device respondsToSelector:@selector(newBinaryArchiveWithDescriptor:error:)]);

        MTLHeapDescriptor *heapDescriptor = [MTLHeapDescriptor new];
        heapDescriptor.size = 1 << 20;
        heapDescriptor.storageMode = MTLStorageModePrivate;
        id<MTLHeap> heap = [device newHeapWithDescriptor:heapDescriptor];
        line("make.heap.private", heap != nil);
        line("make.heap.private.buffer", [heap newBufferWithLength:4096 options:MTLResourceStorageModePrivate] != nil);
        heapDescriptor.storageMode = MTLStorageModeShared;
        line("make.heap.shared", [device newHeapWithDescriptor:heapDescriptor] != nil);

        MTLIndirectCommandBufferDescriptor *icb = [MTLIndirectCommandBufferDescriptor new];
        icb.commandTypes = MTLIndirectCommandTypeDraw;
        icb.maxVertexBufferBindCount = 1;
        line("make.icb.render", [device newIndirectCommandBufferWithDescriptor:icb maxCommandCount:4 options:0] != nil);
        icb.commandTypes = MTLIndirectCommandTypeConcurrentDispatch;
        icb.maxKernelBufferBindCount = 1;
        line("make.icb.compute", [device newIndirectCommandBufferWithDescriptor:icb maxCommandCount:4 options:0] != nil);

        MTLTextureDescriptor *memoryless = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatBGRA8Unorm width:64 height:64 mipmapped:NO];
        memoryless.storageMode = MTLStorageModeMemoryless;
        memoryless.usage = MTLTextureUsageRenderTarget;
        line("make.texture.memoryless", [device newTextureWithDescriptor:memoryless] != nil);
        MTLTextureDescriptor *depth = [MTLTextureDescriptor texture2DDescriptorWithPixelFormat:MTLPixelFormatDepth32Float_Stencil8 width:64 height:64 mipmapped:NO];
        depth.storageMode = MTLStorageModePrivate;
        depth.usage = MTLTextureUsageRenderTarget;
        line("make.texture.d32s8", [device newTextureWithDescriptor:depth] != nil);
        if (@available(macOS 13.0, *)) {
            MTLRasterizationRateMapDescriptor *rate = [MTLRasterizationRateMapDescriptor rasterizationRateMapDescriptorWithScreenSize:MTLSizeMake(64, 64, 0)];
            line("make.rateMap", [device newRasterizationRateMapWithDescriptor:rate] != nil);
        }
    }
    return 0;
}
