/// reims-vgpu-video's three bundles, in kext/ under their own names, which load as one: the
/// VideoToolbox and scaler devices' guest drivers, and the Lilu plugin their x86-64 guests need.
/// build.rs and video.rs both include this list.
pub const VIDEO_SET: [&str; 3] = [
    "AppleVideoToolboxParavirtualization.kext",
    "AppleM2ScalerParavirtDriver.kext",
    "VtWaitPatch.kext",
];
