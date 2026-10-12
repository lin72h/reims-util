//! OpenCore's `config.plist`, made from the `Sample.plist` of the release it boots with, so its
//! keys are always that release's.
//!
//! What changes from the sample is what a q35 VM on KVM needs, following the configuration the
//! reims-vgpu x86-64 guests were measured with (docs/qemu.md in reims-vgpu): an image built with
//! ULTMOS, whose kexts and settings for hardware a VM does not have (Bluetooth, audio codecs, GPU
//! power management, USB port maps, Intel graphics) are left out:
//! - the drivers a VM boots with: OpenRuntime, OpenHfsPlus (recovery images and installers are
//!   HFS+), OpenPartitionDxe (older recovery images' Apple partition maps) and ResetNvramEntry;
//! - no kexts: QEMU's `isa-applesmc` is the SMC, and the GPU is reims-vgpu's, which macOS drives
//!   with its own driver;
//! - one ACPI table, the `EC` and `USBX` devices macOS looks for (acpi.rs);
//! - CPUID leaf 1 reported as a Skylake-SP (family 6, model 85, stepping 4), a CPU every x86-64
//!   macOS from 10.13 to 26 knows, whatever `-cpu` model the VM has;
//! - the kernel quirks a hypervisor needs (`ProvideCurrentCpuInfo`, `DummyPowerManagement`,
//!   `ForceSecureBootScheme`), with Apple Secure Boot off, any volume scanned, no vault;
//! - optionally, the kernel's `kern.hv_vmm_present` swapped with `kern.hibernatecount`, so macOS
//!   does not see it runs in a VM and lets iCloud and the App Store sign in;
//! - the SMBIOS identity (smbios.rs), and the picker's timeout and boot arguments.

use super::smbios::Smbios;
use anyhow::{Context, Result, bail};
use plist::{Dictionary, Value};

/// The drivers the image carries and loads, in order.
pub const DRIVERS: &[&str] = &[
    "OpenRuntime.efi",
    "OpenHfsPlus.efi",
    "OpenPartitionDxe.efi",
    "ResetNvramEntry.efi",
];

/// Arguments for every boot: kernel symbols in panic logs, and no panics on a TLB shootdown or a
/// timeout a vCPU took because the host descheduled it.
pub const DEFAULT_BOOT_ARGS: &str = "keepsyms=1 tlbto_us=0 vti=9";

/// Apple's NVRAM variables' GUID.
const APPLE_NVRAM: &str = "7C436110-AB2A-4BBB-A880-FE41995C9F82";

#[derive(Clone, Debug)]
pub struct Options {
    pub smbios: Smbios,
    /// Seconds the picker waits before it boots the default entry; 0 waits for a choice.
    pub timeout: u32,
    pub boot_args: String,
    /// System Integrity Protection on (`csr-active-config` 0), or off (0x67, as the measured
    /// guests ran).
    pub sip: bool,
    pub cpuid_spoof: bool,
    pub hide_vm: bool,
}

pub fn make(sample: &[u8], o: &Options) -> Result<Vec<u8>> {
    let mut root: Value = plist::from_bytes(sample).context("read Sample.plist")?;
    let c = root
        .as_dictionary_mut()
        .context("Sample.plist is not a dictionary")?;

    // ACPI: our table alone.
    let acpi_entry = {
        let mut e = template(c, &["ACPI", "Add"])?;
        e.insert(
            "Comment".into(),
            "EC and USBX devices macOS looks for".into(),
        );
        e.insert("Enabled".into(), true.into());
        e.insert("Path".into(), super::acpi::FILE_NAME.into());
        e
    };
    set(c, &["ACPI"], "Add", vec![acpi_entry.into()].into())?;
    set(c, &["ACPI"], "Delete", Value::Array(vec![]))?;
    set(c, &["ACPI"], "Patch", Value::Array(vec![]))?;

    set(c, &["Booter"], "MmioWhitelist", Value::Array(vec![]))?;
    set(c, &["Booter"], "Patch", Value::Array(vec![]))?;
    set(
        c,
        &["Booter", "Quirks"],
        "FixupAppleEfiImages",
        false.into(),
    )?;
    set(c, &["Booter", "Quirks"], "SetupVirtualMap", false.into())?;

    set(
        c,
        &["DeviceProperties"],
        "Add",
        Value::Dictionary(Dictionary::new()),
    )?;
    set(
        c,
        &["DeviceProperties"],
        "Delete",
        Value::Dictionary(Dictionary::new()),
    )?;

    set(c, &["Kernel"], "Add", Value::Array(vec![]))?;
    set(c, &["Kernel"], "Block", Value::Array(vec![]))?;
    set(c, &["Kernel"], "Force", Value::Array(vec![]))?;
    let (data, mask) = if o.cpuid_spoof {
        (
            [0x54, 0x06, 0x05, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0].to_vec(),
            [0xff, 0xff, 0xff, 0x00, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0].to_vec(),
        )
    } else {
        (vec![], vec![])
    };
    set(c, &["Kernel", "Emulate"], "Cpuid1Data", Value::Data(data))?;
    set(c, &["Kernel", "Emulate"], "Cpuid1Mask", Value::Data(mask))?;
    set(
        c,
        &["Kernel", "Emulate"],
        "DummyPowerManagement",
        true.into(),
    )?;
    let mut patches = Vec::new();
    if o.hide_vm {
        for (comment, find, replace) in [
            (
                "Hide the VM: kern.hibernatecount reads kern.hv_vmm_present (1 of 2)",
                &b"hibernatehidready\0hibernatecount\0"[..],
                &b"hibernatehidready\0hv_vmm_present\0"[..],
            ),
            (
                "Hide the VM: kern.hv_vmm_present reads kern.hibernatecount (2 of 2)",
                &b"boot session UUID\0hv_vmm_present\0"[..],
                &b"boot session UUID\0hibernatecount\0"[..],
            ),
        ] {
            let mut p = template(c, &["Kernel", "Patch"])?;
            p.insert("Arch".into(), "x86_64".into());
            p.insert("Base".into(), "".into());
            p.insert("Comment".into(), comment.into());
            p.insert("Count".into(), 1.into());
            p.insert("Enabled".into(), true.into());
            p.insert("Find".into(), Value::Data(find.to_vec()));
            p.insert("Identifier".into(), "kernel".into());
            p.insert("Limit".into(), 0.into());
            p.insert("Mask".into(), Value::Data(vec![]));
            p.insert("MaxKernel".into(), "".into());
            p.insert("MinKernel".into(), "".into());
            p.insert("Replace".into(), Value::Data(replace.to_vec()));
            p.insert("ReplaceMask".into(), Value::Data(vec![]));
            p.insert("Skip".into(), 0.into());
            patches.push(Value::Dictionary(p));
        }
    }
    set(c, &["Kernel"], "Patch", Value::Array(patches))?;
    set(
        c,
        &["Kernel", "Quirks"],
        "ForceSecureBootScheme",
        true.into(),
    )?;
    set(
        c,
        &["Kernel", "Quirks"],
        "ProvideCurrentCpuInfo",
        true.into(),
    )?;
    set(c, &["Kernel", "Quirks"], "SetApfsTrimTimeout", 0.into())?;

    set(c, &["Misc", "Boot"], "HideAuxiliary", false.into())?;
    set(c, &["Misc", "Boot"], "PickerMode", "Builtin".into())?;
    set(c, &["Misc", "Boot"], "PollAppleHotKeys", true.into())?;
    set(c, &["Misc", "Boot"], "ShowPicker", true.into())?;
    set(c, &["Misc", "Boot"], "Timeout", (o.timeout as i64).into())?;
    set(c, &["Misc"], "Entries", Value::Array(vec![]))?;
    set(c, &["Misc"], "Tools", Value::Array(vec![]))?;
    set(c, &["Misc", "Security"], "AllowSetDefault", true.into())?;
    set(c, &["Misc", "Security"], "ScanPolicy", 0.into())?;
    set(
        c,
        &["Misc", "Security"],
        "SecureBootModel",
        "Disabled".into(),
    )?;
    set(c, &["Misc", "Security"], "Vault", "Optional".into())?;

    let nvram = &["NVRAM", "Add", APPLE_NVRAM];
    set(c, nvram, "boot-args", o.boot_args.trim().into())?;
    let csr: u32 = if o.sip { 0 } else { 0x67 };
    set(
        c,
        nvram,
        "csr-active-config",
        Value::Data(csr.to_le_bytes().to_vec()),
    )?;
    set(c, nvram, "prev-lang:kbd", Value::Data(b"en-US:0".to_vec()))?;
    // Deleted before they are added, so the image's values win over what NVRAM holds.
    let delete = dict(c, &["NVRAM", "Delete"])?;
    let mut names: Vec<Value> = match delete.get(APPLE_NVRAM) {
        Some(Value::Array(a)) => a.clone(),
        _ => vec![],
    };
    for n in ["boot-args", "csr-active-config", "prev-lang:kbd"] {
        if !names.iter().any(|v| v.as_string() == Some(n)) {
            names.push(n.into());
        }
    }
    delete.insert(APPLE_NVRAM.into(), Value::Array(names));

    let g = &["PlatformInfo", "Generic"];
    set(c, &["PlatformInfo"], "Automatic", true.into())?;
    set(c, g, "SystemProductName", o.smbios.model.as_str().into())?;
    set(c, g, "SystemSerialNumber", o.smbios.serial.as_str().into())?;
    set(c, g, "MLB", o.smbios.mlb.as_str().into())?;
    set(c, g, "SystemUUID", o.smbios.uuid.as_str().into())?;
    set(c, g, "ROM", Value::Data(unhex(&o.smbios.rom)?))?;

    set(c, &["UEFI", "APFS"], "MinDate", (-1i64).into())?;
    set(c, &["UEFI", "APFS"], "MinVersion", (-1i64).into())?;
    set(c, &["UEFI", "Output"], "Resolution", "".into())?;
    let sample_drivers: Vec<Value> = match dict(c, &["UEFI"])?.get("Drivers") {
        Some(Value::Array(a)) => a.clone(),
        _ => bail!("Sample.plist has no UEFI > Drivers"),
    };
    let mut drivers = Vec::new();
    for name in DRIVERS {
        let mut e = sample_drivers
            .iter()
            .filter_map(Value::as_dictionary)
            .find(|d| d.get("Path").and_then(Value::as_string) == Some(name))
            .cloned()
            .or_else(|| {
                sample_drivers
                    .first()
                    .and_then(Value::as_dictionary)
                    .cloned()
            })
            .context("Sample.plist lists no drivers")?;
        e.insert("Path".into(), (*name).into());
        e.insert("Arguments".into(), "".into());
        e.insert("Enabled".into(), true.into());
        e.insert("LoadEarly".into(), false.into());
        drivers.push(Value::Dictionary(e));
    }
    set(c, &["UEFI"], "Drivers", Value::Array(drivers))?;
    set(c, &["UEFI"], "ReservedMemory", Value::Array(vec![]))?;

    let mut out = Vec::new();
    root.to_writer_xml(&mut out).context("write config.plist")?;
    Ok(out)
}

fn dict<'a>(root: &'a mut Dictionary, path: &[&str]) -> Result<&'a mut Dictionary> {
    let mut d = root;
    for (i, k) in path.iter().enumerate() {
        d = d
            .get_mut(k)
            .and_then(Value::as_dictionary_mut)
            .with_context(|| format!("Sample.plist has no {}", path[..=i].join(" > ")))?;
    }
    Ok(d)
}

/// Sets a key that the sample has, so a key a later release renamed fails here, not at boot.
fn set(root: &mut Dictionary, path: &[&str], key: &str, value: Value) -> Result<()> {
    let d = dict(root, path)?;
    if !d.contains_key(key) {
        bail!("Sample.plist has no {} > {key}", path.join(" > "));
    }
    d.insert(key.into(), value);
    Ok(())
}

/// The first entry of an array of dictionaries: a new entry has its keys.
fn template(root: &mut Dictionary, path: &[&str]) -> Result<Dictionary> {
    let (parent, key) = path.split_at(path.len() - 1);
    let d = dict(root, parent)?;
    d.get(key[0])
        .and_then(Value::as_array)
        .and_then(|a| a.first())
        .and_then(Value::as_dictionary)
        .cloned()
        .with_context(|| format!("Sample.plist has no entry in {}", path.join(" > ")))
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    (0..s.len())
        .step_by(2)
        .map(|i| {
            s.get(i..i + 2)
                .and_then(|b| u8::from_str_radix(b, 16).ok())
                .with_context(|| format!("not hex: {s}"))
        })
        .collect()
}
