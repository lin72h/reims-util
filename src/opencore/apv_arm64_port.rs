//! The apv-arm64-port kexts: seven bundles, which load as one, after Lilu and ReimsVgpu, on macOS
//! 26.4 and later. kext/ holds them as built, under their own names: the apv-arm64-port project's
//! deploy.sh writes them there, and build.rs embeds them. Every image carries them unless kext/
//! holds none of them; `--no-apv-arm64-port` makes one without them.

use anyhow::{Context, Result, bail};
use std::collections::{BTreeMap, BTreeSet};

include!(concat!(env!("OUT_DIR"), "/apv_arm64_port.rs"));

include!("apv_arm64_port_set.rs");

/// A kext of the set.
#[derive(Debug)]
pub struct Kext<'a> {
    pub bundle: &'a str,
    /// Its executable in `Contents/MacOS`, or None for a kext with no code.
    pub executable: Option<String>,
    identifier: String,
    libraries: Vec<String>,
    /// Its files, by their path in the bundle.
    pub files: Vec<(&'a str, &'a [u8])>,
}

/// The set in `files` (each by its path under kext/), in the order OpenCore loads it: each kext
/// after those of the set its `OSBundleLibraries` names. Empty when `files` holds no kext; files
/// beside the bundles are left out, and anything short of the exact set refused.
pub fn set<'a>(files: &[(&'a str, &'a [u8])]) -> Result<Vec<Kext<'a>>> {
    set_of(files, &SET, "apv-arm64-port", "all seven")
}

/// The kexts `names` in `files`, taken as `set` takes apv-arm64-port's: `what` names them in its
/// errors, and `whole` says how many make the set. video.rs takes reims-vgpu-video's kexts so.
pub(super) fn set_of<'a>(
    files: &[(&'a str, &'a [u8])],
    names: &[&str],
    what: &str,
    whole: &str,
) -> Result<Vec<Kext<'a>>> {
    let mut bundles: BTreeMap<&str, Vec<(&str, &[u8])>> = BTreeMap::new();
    for &(path, data) in files {
        let Some((bundle, rel)) = path.split_once('/') else {
            continue;
        };
        if !bundle.ends_with(".kext") || path.split('/').any(|c| c.starts_with('.')) {
            continue;
        }
        if !names.contains(&bundle) {
            bail!(
                "kext/{bundle} is not one of the {what} kexts ({})",
                names.join(", ")
            );
        }
        bundles.entry(bundle).or_default().push((rel, data));
    }
    if bundles.is_empty() {
        return Ok(Vec::new());
    }
    let missing: Vec<&str> = names
        .iter()
        .copied()
        .filter(|b| !bundles.contains_key(b))
        .collect();
    if !missing.is_empty() {
        bail!(
            "partial {what} set in kext/, without {}: deploy {whole}",
            missing.join(", ")
        );
    }
    let mut kexts = Vec::new();
    for (bundle, files) in bundles {
        kexts.push(kext(bundle, files)?);
    }
    order(kexts, what)
}

fn kext<'a>(bundle: &'a str, files: Vec<(&'a str, &'a [u8])>) -> Result<Kext<'a>> {
    let plist = files
        .iter()
        .find(|(p, _)| *p == "Contents/Info.plist")
        .with_context(|| format!("kext/{bundle} has no Contents/Info.plist"))?;
    let info: plist::Dictionary = plist::from_bytes(plist.1)
        .with_context(|| format!("read kext/{bundle}/Contents/Info.plist"))?;
    let string = |k: &str| info.get(k).and_then(plist::Value::as_string);
    let identifier = string("CFBundleIdentifier")
        .with_context(|| format!("kext/{bundle} has no CFBundleIdentifier"))?
        .to_string();
    let executable = string("CFBundleExecutable").map(str::to_string);
    if let Some(e) = &executable {
        let path = format!("Contents/MacOS/{e}");
        if !files.iter().any(|(p, _)| *p == path) {
            bail!("kext/{bundle} has no {path}, the executable its Info.plist names");
        }
    }
    let libraries = info
        .get("OSBundleLibraries")
        .and_then(plist::Value::as_dictionary)
        .map(|d| d.keys().cloned().collect())
        .unwrap_or_default();
    Ok(Kext {
        bundle,
        executable,
        identifier,
        libraries,
        files,
    })
}

/// Orders the kexts so each comes after the ones it links against, and otherwise by name.
fn order<'a>(mut left: Vec<Kext<'a>>, what: &str) -> Result<Vec<Kext<'a>>> {
    let ids: BTreeSet<String> = left.iter().map(|k| k.identifier.clone()).collect();
    let mut placed = BTreeSet::new();
    let mut out = Vec::new();
    while !left.is_empty() {
        let ready = left.iter().position(|k| {
            k.libraries
                .iter()
                .all(|l| !ids.contains(l) || placed.contains(l))
        });
        let Some(i) = ready else {
            let names: Vec<&str> = left.iter().map(|k| k.bundle).collect();
            bail!(
                "the {what} kexts {} link against each other in a cycle",
                names.join(", ")
            );
        };
        let k = left.remove(i);
        placed.insert(k.identifier.clone());
        out.push(k);
    }
    Ok(out)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A bundle's Info.plist: its identifier, executable and libraries.
    pub(crate) fn info(id: &str, executable: Option<&str>, libraries: &[&str]) -> Vec<u8> {
        let mut d = plist::Dictionary::new();
        d.insert("CFBundleIdentifier".into(), id.into());
        if let Some(e) = executable {
            d.insert("CFBundleExecutable".into(), e.into());
        }
        let mut l = plist::Dictionary::new();
        for x in libraries {
            l.insert(x.to_string(), "1.0.0".into());
        }
        d.insert("OSBundleLibraries".into(), l.into());
        let mut v = Vec::new();
        plist::Value::Dictionary(d).to_writer_xml(&mut v).unwrap();
        v
    }

    /// The seven, linked as the set is: each kext on its bridges, the loader on Lilu. The
    /// bridges here have no code, to cover a kext without an executable.
    pub(crate) fn fixture() -> Vec<(String, Vec<u8>)> {
        let lilu = "as.vit9696.Lilu";
        let kexts: [(&str, &str, bool, &[&str]); 7] = [
            ("AppleParavirtGPULoader", "loader", true, &[lilu]),
            ("IOGPUFamilyBridge", "gpub", false, &["com.apple.kpi.iokit"]),
            ("IOGraphicsFamilyBridge", "gfxb", false, &[]),
            ("AppleGraphicsDeviceControlBridge", "agdcb", false, &[]),
            ("AppleParavirtIOSurface", "iosurface", true, &[]),
            (
                "AppleParavirtGPUIOGPUFamily",
                "gpu",
                true,
                &["t.gpub", "t.iosurface"],
            ),
            (
                "APVDisplayPort",
                "display",
                true,
                &["t.gfxb", "t.agdcb", "t.gpu"],
            ),
        ];
        let mut files = Vec::new();
        for (name, id, code, libraries) in kexts {
            let exe = code.then_some(name);
            files.push((
                format!("{name}.kext/Contents/Info.plist"),
                info(&format!("t.{id}"), exe, libraries),
            ));
            if code {
                files.push((format!("{name}.kext/Contents/MacOS/{name}"), vec![0xcf]));
            }
        }
        files
    }

    pub(crate) fn refs(files: &[(String, Vec<u8>)]) -> Vec<(&str, &[u8])> {
        files
            .iter()
            .map(|(p, d)| (p.as_str(), d.as_slice()))
            .collect()
    }

    fn bundles(kexts: &[Kext<'_>]) -> Vec<String> {
        kexts
            .iter()
            .map(|k| k.bundle.replace(".kext", ""))
            .collect()
    }

    #[test]
    fn ordered_by_libraries() {
        let files = fixture();
        let kexts = set(&refs(&files)).unwrap();
        assert_eq!(
            bundles(&kexts),
            [
                "AppleGraphicsDeviceControlBridge",
                "AppleParavirtGPULoader",
                "AppleParavirtIOSurface",
                "IOGPUFamilyBridge",
                "AppleParavirtGPUIOGPUFamily",
                "IOGraphicsFamilyBridge",
                "APVDisplayPort",
            ]
        );
    }

    #[test]
    fn executables() {
        let files = fixture();
        let kexts = set(&refs(&files)).unwrap();
        for k in &kexts {
            let code = !k.bundle.ends_with("Bridge.kext");
            assert_eq!(
                k.executable.as_deref(),
                code.then(|| k.bundle.trim_end_matches(".kext")),
                "{}",
                k.bundle
            );
            assert_eq!(k.files.len(), if code { 2 } else { 1 }, "{}", k.bundle);
        }

        let mut files = fixture();
        files.retain(|(p, _)| p != "APVDisplayPort.kext/Contents/MacOS/APVDisplayPort");
        let e = set(&refs(&files)).unwrap_err().to_string();
        assert!(e.contains("Contents/MacOS/APVDisplayPort"), "{e}");
    }

    #[test]
    fn side_files_left_out() {
        let mut files = fixture();
        for p in [
            "notes.txt",
            "APVDisplayPort.plist",
            "APVDisplayPort.kext/.DS_Store",
            "APVDisplayPort.kext/Contents/._Info.plist",
        ] {
            files.push((p.to_string(), b"junk".to_vec()));
        }
        let kexts = set(&refs(&files)).unwrap();
        assert_eq!(kexts.len(), 7);
        for k in &kexts {
            for (p, _) in &k.files {
                assert!(!p.contains("/.") && !p.starts_with('.'), "{}/{p}", k.bundle);
            }
        }
    }

    #[test]
    fn other_kexts_refused() {
        for extra in ["Lilu.kext", "IOGPUFamily.kext"] {
            let mut files = fixture();
            files.push((format!("{extra}/Contents/Info.plist"), info("x", None, &[])));
            let e = set(&refs(&files)).unwrap_err().to_string();
            assert!(e.contains(extra), "{e}");
        }
    }

    #[test]
    fn part_of_the_set_refused() {
        let mut files = fixture();
        files.retain(|(p, _)| !p.starts_with("IOGPUFamilyBridge.kext/"));
        let e = set(&refs(&files)).unwrap_err().to_string();
        assert!(e.contains("without IOGPUFamilyBridge.kext"), "{e}");
    }

    #[test]
    fn cycle_refused() {
        let mut files = fixture();
        for (p, d) in &mut files {
            if p == "AppleParavirtIOSurface.kext/Contents/Info.plist" {
                *d = info(
                    "t.iosurface",
                    Some("AppleParavirtIOSurface"),
                    &["t.display"],
                );
            }
        }
        let e = set(&refs(&files)).unwrap_err().to_string();
        assert!(e.contains("cycle"), "{e}");
    }

    #[test]
    fn nothing_without_kexts() {
        assert!(set(&[]).unwrap().is_empty());
        let side: [(&str, &[u8]); 1] = [("notes.txt", b"")];
        assert!(set(&side).unwrap().is_empty());
    }

    /// What kext/ holds of the set is the whole of it, or nothing.
    #[test]
    fn embedded_set_whole() {
        let kexts = set(FILES).unwrap();
        assert!(kexts.is_empty() || kexts.len() == SET.len());
    }
}
