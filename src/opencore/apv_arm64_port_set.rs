/// The seven apv-arm64-port bundles, in kext/ under their own names, which load as one:
/// AppleParavirtGPULoader binds the others through the bridges, and refuses when one is missing
/// or differs. build.rs and apv_arm64_port.rs both include this list.
pub const SET: [&str; 7] = [
    "AppleParavirtGPULoader.kext",
    "IOGPUFamilyBridge.kext",
    "IOGraphicsFamilyBridge.kext",
    "AppleGraphicsDeviceControlBridge.kext",
    "AppleParavirtIOSurface.kext",
    "AppleParavirtGPUIOGPUFamily.kext",
    "APVDisplayPort.kext",
];
