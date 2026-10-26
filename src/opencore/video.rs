//! reims-vgpu-video's kexts: three bundles, which load as one, after the apv-arm64-port kexts, on
//! macOS 26.7 (Darwin 25.6) alone. kext/ holds them as built, under their own names: the
//! reims-vgpu-video project builds them (its docs/kext-port.md), and build.rs embeds them.
//! - AppleVideoToolboxParavirtualization.kext, the guest driver of the paravirtualized
//!   VideoToolbox device (106b:1a03), Apple's arm64 kext ported to x86-64;
//! - AppleM2ScalerParavirtDriver.kext, the M2 scaler device's (106b:1a0b), ported the same way;
//! - VtWaitPatch.kext, a Lilu plugin of reims-vgpu-video's own, which patches the x86-64 guest's
//!   VideoToolbox so that its 10 s waits for the host's replies do not expire at once. It patches
//!   macOS 26.7.1 (25G241) alone, and leaves any other build as it is.
//!
//! The two drivers link against apv-arm64-port's IOSurface mapper, so an image carries them only
//! with the apv-arm64-port kexts, and only where kext/ holds them; `--no-video` makes one without
//! them. A driver whose device the VM lacks loads and matches nothing.

use super::apv_arm64_port::{Kext, set_of};
use anyhow::Result;

include!(concat!(env!("OUT_DIR"), "/video.rs"));

include!("video_set.rs");

/// The video kexts in `files` (each by its path under kext/), in the order OpenCore loads them.
/// Empty when `files` holds none of them; anything short of the three refused.
pub fn set<'a>(files: &[(&'a str, &'a [u8])]) -> Result<Vec<Kext<'a>>> {
    set_of(files, &VIDEO_SET, "video", "all three")
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::opencore::apv_arm64_port::tests::{info, refs};

    /// The three, linked as they are: the drivers against the mapper and Apple's families, the
    /// plugin against Lilu, none against each other.
    pub(crate) fn fixture() -> Vec<(String, Vec<u8>)> {
        let mapper = "com.apple.iokit.AppleParavirtIOSurface";
        let kexts: [(&str, &str, &[&str]); 3] = [
            (
                "AppleVideoToolboxParavirtualization",
                "com.apple.driver.AppleVideoToolboxParavirtualization",
                &[mapper, "com.apple.driver.AppleVirtIO"],
            ),
            (
                "AppleM2ScalerParavirtDriver",
                "com.apple.driver.AppleM2ScalerParavirtDriver",
                &[mapper, "com.apple.iokit.IOPCIFamily"],
            ),
            ("VtWaitPatch", "t.vtwait", &["as.vit9696.Lilu"]),
        ];
        let mut files = Vec::new();
        for (name, id, libraries) in kexts {
            files.push((
                format!("{name}.kext/Contents/Info.plist"),
                info(id, Some(name), libraries),
            ));
            files.push((format!("{name}.kext/Contents/MacOS/{name}"), vec![0xcf]));
        }
        files
    }

    #[test]
    fn ordered_by_name() {
        let files = fixture();
        let kexts = set(&refs(&files)).unwrap();
        let bundles: Vec<&str> = kexts.iter().map(|k| k.bundle).collect();
        assert_eq!(
            bundles,
            [
                "AppleM2ScalerParavirtDriver.kext",
                "AppleVideoToolboxParavirtualization.kext",
                "VtWaitPatch.kext",
            ]
        );
        for k in &kexts {
            assert_eq!(
                k.executable.as_deref(),
                Some(k.bundle.trim_end_matches(".kext"))
            );
        }
    }

    #[test]
    fn part_of_the_set_refused() {
        let mut files = fixture();
        files.retain(|(p, _)| !p.starts_with("VtWaitPatch.kext/"));
        let e = set(&refs(&files)).unwrap_err().to_string();
        assert!(e.contains("without VtWaitPatch.kext"), "{e}");
    }

    #[test]
    fn other_kexts_refused() {
        let mut files = fixture();
        files.push((
            "AppleParavirtIOSurface.kext/Contents/Info.plist".into(),
            info("x", None, &[]),
        ));
        let e = set(&refs(&files)).unwrap_err().to_string();
        assert!(e.contains("not one of the video kexts"), "{e}");
    }

    /// What kext/ holds of the three is the whole of it, or nothing.
    #[test]
    fn embedded_set_whole() {
        let kexts = set(FILES).unwrap();
        assert!(kexts.is_empty() || kexts.len() == VIDEO_SET.len());
    }
}
