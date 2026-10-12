//! The OpenCore boot image: a 128 MiB GPT disk whose EFI system partition holds a pinned
//! OpenCore release, the drivers and ACPI table a VM needs, and a `config.plist` made for a q35
//! VM (config.rs) with an SMBIOS identity of its own (smbios.rs). OVMF goes beside it (ovmf.rs),
//! where reims-ctl looks for it.

pub mod acpi;
pub mod config;
pub mod smbios;

use crate::cache::{self, Pinned};
use crate::image::{self, Data, Format};
use crate::progress::{self, Reporter};
use anyhow::{Context, Result};
use std::io::Read;
use std::path::{Path, PathBuf};

pub const VERSION: &str = "1.0.8";

const RELEASE: Pinned = Pinned {
    name: "OpenCore-1.0.8-RELEASE.zip",
    url: "https://github.com/acidanthera/OpenCorePkg/releases/download/1.0.8/OpenCore-1.0.8-RELEASE.zip",
    sha256: "2011e8b7216ecb2645d97ea710df965947edd406846e92050b9bc4190be6d27b",
    what: "OpenCore 1.0.8",
};

/// The image's size: room for OpenCore (about 2 MiB of it here) and for what a person may add.
const DISK_SIZE: u64 = 128 << 20;

#[derive(Clone, Debug)]
pub struct Options {
    pub output: PathBuf,
    pub config: config::Options,
    /// Where OVMF's files go, or None to write none.
    pub ovmf_dir: Option<PathBuf>,
}

#[derive(Clone, Debug, serde::Serialize)]
pub struct Outputs {
    pub image: PathBuf,
    pub format: Format,
    pub opencore: &'static str,
    pub smbios: smbios::Smbios,
    pub ovmf: Option<crate::ovmf::Outputs>,
}

pub fn build(o: &Options, r: &dyn Reporter) -> Result<Outputs> {
    let zip_path = cache::fetch(&RELEASE, r)?;
    progress::check(r)?;
    r.step(&format!("Building the OpenCore {VERSION} image"));
    let files = files(&zip_path, &o.config)?;
    let files: Vec<(String, Data)> = files
        .into_iter()
        .map(|(p, d)| (p, Data::Bytes(d)))
        .collect();
    let disk = image::esp_disk(DISK_SIZE, "OPENCORE", &files)?;
    let format = Format::for_path(&o.output);
    image::write(&disk, &o.output, format)?;
    r.note(&format!("Wrote {}", o.output.display()));
    let ovmf = match &o.ovmf_dir {
        Some(dir) => {
            progress::check(r)?;
            let out = crate::ovmf::write(dir, r)?;
            r.note(&format!(
                "Wrote {} and {}",
                out.code.display(),
                out.vars.display()
            ));
            Some(out)
        }
        None => None,
    };
    Ok(Outputs {
        image: o.output.clone(),
        format,
        opencore: VERSION,
        smbios: o.config.smbios.clone(),
        ovmf,
    })
}

/// The partition's files, from the release's X64 tree, and the made config.plist and ACPI table.
fn files(zip_path: &Path, c: &config::Options) -> Result<Vec<(String, Vec<u8>)>> {
    let f =
        std::fs::File::open(zip_path).with_context(|| format!("open {}", zip_path.display()))?;
    let mut zip = zip::ZipArchive::new(f).context("read the OpenCore release")?;
    let mut read = |name: &str| -> Result<Vec<u8>> {
        let mut e = zip
            .by_name(name)
            .with_context(|| format!("the OpenCore release has no {name}"))?;
        let mut v = Vec::new();
        e.read_to_end(&mut v)?;
        Ok(v)
    };
    let mut files = Vec::new();
    for p in [
        "EFI/BOOT/BOOTx64.efi",
        "EFI/BOOT/.contentFlavour",
        "EFI/BOOT/.contentVisibility",
        "EFI/OC/OpenCore.efi",
        "EFI/OC/.contentFlavour",
        "EFI/OC/.contentVisibility",
    ] {
        files.push((p.to_string(), read(&format!("X64/{p}"))?));
    }
    for d in config::DRIVERS {
        files.push((
            format!("EFI/OC/Drivers/{d}"),
            read(&format!("X64/EFI/OC/Drivers/{d}"))?,
        ));
    }
    let sample = read("Docs/Sample.plist")?;
    files.push(("EFI/OC/config.plist".into(), config::make(&sample, c)?));
    files.push((
        format!("EFI/OC/ACPI/{}", acpi::FILE_NAME),
        acpi::ssdt_ec_usbx(),
    ));
    // The release's other folders, empty, for what a person adds.
    for d in ["EFI/OC/Kexts/", "EFI/OC/Resources/", "EFI/OC/Tools/"] {
        files.push((d.to_string(), Vec::new()));
    }
    Ok(files)
}
