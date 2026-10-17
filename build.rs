//! Embeds the apv-arm64-port kexts, the seven bundles in kext/ that `SET` names, so the image
//! carries them as it carries ReimsVgpu.kext (src/opencore/apv_arm64_port.rs). Writes
//! `$OUT_DIR/apv_arm64_port.rs`: each file of each of them, by its path under kext/, with its
//! bytes, dotfiles left out. Nothing else in kext/ is read. With none of the seven there, nothing
//! is embedded; with only some, the build fails, as the seven load as one.
//!
//! The bundles are built without a path of the machine they were built on: the build fails when
//! an embedded file holds `/Users/`, as an unstripped kext's debug map does.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

include!("src/opencore/apv_arm64_port_set.rs");

fn main() {
    if let Err(e) = run() {
        eprintln!("apv-arm64-port: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").map_err(|e| e.to_string())?);
    let dir = root.join("kext");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/opencore/apv_arm64_port_set.rs");
    println!("cargo:rerun-if-changed=kext");

    let present: Vec<&str> = SET
        .iter()
        .copied()
        .filter(|b| dir.join(b).exists())
        .collect();
    if !present.is_empty() && present.len() != SET.len() {
        let missing: Vec<&str> = SET
            .iter()
            .copied()
            .filter(|b| !present.contains(b))
            .collect();
        return Err(format!(
            "partial apv-arm64-port set in kext/, without {}: deploy all seven",
            missing.join(", ")
        ));
    }
    let mut files = Vec::new();
    for b in &present {
        walk(&dir, Path::new(b), &mut files)?;
    }
    files.sort();

    let mut code = String::from(
        "/// Each file of the apv-arm64-port kexts, by its path under kext/ (build.rs).\n\
         pub static FILES: &[(&str, &[u8])] = &[\n",
    );
    for f in &files {
        let path = dir.join(f);
        println!("cargo:rerun-if-changed={}", path.display());
        let bytes = std::fs::read(&path).map_err(|e| format!("read {}: {e}", path.display()))?;
        if bytes.windows(7).any(|w| w == b"/Users/") {
            return Err(format!(
                "kext/{f} holds a /Users/ path, from the machine it was built on: deploy a \
                 stripped build"
            ));
        }
        let abs = path
            .to_str()
            .ok_or(format!("{} is not UTF-8", path.display()))?;
        let _ = writeln!(code, "    ({f:?}, include_bytes!({abs:?})),");
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").map_err(|e| e.to_string())?);
    std::fs::write(out.join("apv_arm64_port.rs"), code)
        .map_err(|e| format!("write apv_arm64_port.rs: {e}"))
}

fn read_dir(dir: &Path) -> Result<Vec<std::fs::DirEntry>, String> {
    let mut v = std::fs::read_dir(dir)
        .and_then(|r| r.collect::<Result<Vec<_>, _>>())
        .map_err(|e| format!("read {}: {e}", dir.display()))?;
    v.sort_by_key(|e| e.file_name());
    Ok(v)
}

/// Adds the files under `dir/rel`, by their path under `dir`, skipping dotfiles.
fn walk(dir: &Path, rel: &Path, files: &mut Vec<String>) -> Result<(), String> {
    let path = dir.join(rel);
    let t = std::fs::symlink_metadata(&path)
        .map_err(|e| format!("read {}: {e}", path.display()))?
        .file_type();
    if t.is_dir() {
        for e in read_dir(&path)? {
            if !e.file_name().to_string_lossy().starts_with('.') {
                walk(dir, &rel.join(e.file_name()), files)?;
            }
        }
    } else if t.is_file() {
        let s = rel
            .to_str()
            .ok_or(format!("{} is not UTF-8", rel.display()))?;
        files.push(s.to_string());
    } else {
        return Err(format!("{} is neither a file nor a folder", rel.display()));
    }
    Ok(())
}
