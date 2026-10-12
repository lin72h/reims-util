//! A FAT32 volume holding a list of files, as Microsoft's FAT specification (FAT: General
//! Overview of On-Disk Format, 1.03) lays it out:
//! - 32 reserved sectors: the boot sector, the FSInfo sector, and their backups at 6 and 7;
//! - two FATs, sized by the specification's formula;
//! - the data region, cluster 2 first, which is the root directory.
//!
//! Clusters are the size the specification's table gives the volume's (512 bytes up to 260 MiB,
//! 4 KiB up to 8 GiB). Each folder and file takes consecutive clusters, folders first, in the
//! order given. Names that are not plain upper-case 8.3 names get long-name entries, with a
//! `~1`-style short name. The volume is made as the byte ranges it holds (image/mod.rs).

use super::Data;
use anyhow::{Result, bail, ensure};

const SECTOR: usize = 512;
const RESERVED: u32 = 32;
const FATS: u32 = 2;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_VOLUME_ID: u8 = 0x08;
const ATTR_LONG_NAME: u8 = 0x0f;
const EOC: u32 = 0x0fff_ffff;

/// A folder's contents, by name: a file's data, or a folder.
#[derive(Default)]
struct Dir {
    entries: Vec<(String, Node)>,
}

enum Node {
    File(Data),
    Dir(Dir),
}

impl Dir {
    fn insert(&mut self, path: &[&str], data: Option<Data>) -> Result<()> {
        let Some((first, rest)) = path.split_first() else {
            return Ok(());
        };
        let found = self
            .entries
            .iter_mut()
            .find(|(n, _)| n.eq_ignore_ascii_case(first));
        if rest.is_empty() {
            match (found, data) {
                (None, Some(d)) => self.entries.push((first.to_string(), Node::File(d))),
                (None, None) => self
                    .entries
                    .push((first.to_string(), Node::Dir(Dir::default()))),
                (Some((_, Node::Dir(_))), None) => {}
                (Some(_), _) => bail!("{first} is given twice"),
            }
            return Ok(());
        }
        match found {
            Some((_, Node::Dir(d))) => d.insert(rest, data),
            Some(_) => bail!("{first} is a file and a folder"),
            None => {
                let mut d = Dir::default();
                d.insert(rest, data)?;
                self.entries.push((first.to_string(), Node::Dir(d)));
                Ok(())
            }
        }
    }
}

/// A volume of `size` bytes (a whole partition, in 512-byte sectors) holding `files` at their
/// slash-separated paths, as byte ranges from the volume's start; a path ending in `/` is an
/// empty folder. `hidden_sectors` is where the partition starts on its disk; `label` is up to 11
/// characters; `serial` the volume ID.
pub fn make(
    size: u64,
    hidden_sectors: u32,
    label: &str,
    serial: u32,
    files: &[(String, Data)],
    now: (u16, u16),
) -> Result<Vec<(u64, Data)>> {
    let mut root = Dir::default();
    for (path, data) in files {
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        ensure!(!parts.is_empty(), "empty path");
        let data = if path.ends_with('/') {
            None
        } else {
            ensure!(
                data.size() <= u64::from(u32::MAX),
                "{path} is 4 GiB or more, which FAT32 cannot hold"
            );
            Some(data.clone())
        };
        root.insert(&parts, data)?;
    }

    ensure!(
        size.is_multiple_of(SECTOR as u64),
        "the volume is not whole sectors"
    );
    let total = u32::try_from(size / SECTOR as u64)?;
    let per_cluster = sectors_per_cluster(total);
    let cluster_bytes = u64::from(per_cluster) * SECTOR as u64;
    let fat_size = {
        let tmp1 = total - RESERVED;
        let tmp2 = (256 * per_cluster + FATS) / 2;
        tmp1.div_ceil(tmp2)
    };
    let data_start = RESERVED + FATS * fat_size;
    let clusters = (total - data_start) / per_cluster;
    ensure!(
        (65525..268_435_445).contains(&clusters),
        "{clusters} clusters is not a FAT32 volume"
    );

    // Lay the tree out: each folder's entries, and the clusters of every folder and file.
    let mut fat = vec![0u32; clusters as usize + 2];
    fat[0] = 0x0fff_fff8;
    fat[1] = EOC;
    let mut next = 2u32;
    let mut alloc = |bytes: u64| -> Result<u32> {
        let n = u32::try_from(bytes.div_ceil(cluster_bytes).max(1))?;
        let first = next;
        ensure!(
            u64::from(first) + u64::from(n) <= u64::from(clusters) + 2,
            "the files do not fit the volume"
        );
        for c in first..first + n {
            fat[c as usize] = if c + 1 == first + n { EOC } else { c + 1 };
        }
        next += n;
        Ok(first)
    };
    let mut writes: Vec<(u32, Data)> = Vec::new();
    lay_out(&root, 0, true, label, now, &mut alloc, &mut writes)?;
    let used = next - 2;

    let sector = |n: u32| u64::from(n) * SECTOR as u64;
    let bs = boot_sector(total, per_cluster, fat_size, hidden_sectors, label, serial);
    let fsinfo = fsinfo(clusters - used, next);
    // Each structure fills its sectors or clusters, so none keeps what an earlier image left.
    let mut fat: Vec<u8> = fat.iter().flat_map(|v| v.to_le_bytes()).collect();
    fat.resize(fat_size as usize * SECTOR, 0);
    let mut out = vec![
        (0, Data::Bytes(bs.to_vec())),
        (sector(1), Data::Bytes(fsinfo.to_vec())),
        (sector(6), Data::Bytes(bs.to_vec())),
        (sector(7), Data::Bytes(fsinfo.to_vec())),
    ];
    for f in 0..FATS {
        out.push((sector(RESERVED + f * fat_size), Data::Bytes(fat.clone())));
    }
    for (cluster, data) in writes {
        let data = match data {
            Data::Bytes(mut b) => {
                b.resize(b.len().next_multiple_of(cluster_bytes as usize), 0);
                Data::Bytes(b)
            }
            reserved => reserved,
        };
        out.push((sector(data_start + (cluster - 2) * per_cluster), data));
    }
    Ok(out)
}

/// Sectors per cluster, by the specification's table for FAT32 volumes of `total` sectors.
fn sectors_per_cluster(total: u32) -> u32 {
    match total {
        0..=532_480 => 1,
        532_481..=16_777_216 => 8,
        16_777_217..=33_554_432 => 16,
        33_554_433..=67_108_864 => 32,
        _ => 64,
    }
}

/// Allocates `dir` (at the root when `root`) and everything under it, depth first, and queues
/// what each cluster run holds. Returns the folder's first cluster.
fn lay_out(
    dir: &Dir,
    parent: u32,
    root: bool,
    label: &str,
    now: (u16, u16),
    alloc: &mut dyn FnMut(u64) -> Result<u32>,
    writes: &mut Vec<(u32, Data)>,
) -> Result<u32> {
    // The folder's size first: its clusters come before its children's.
    let mut shorts: Vec<[u8; 11]> = Vec::new();
    let mut names = Vec::new();
    for (name, _) in &dir.entries {
        let (short, long) = short_name(name, &shorts)?;
        shorts.push(short);
        names.push((short, long));
    }
    let count = usize::from(root)
        + if root { 0 } else { 2 }
        + names
            .iter()
            .map(|(_, l)| 1 + l.as_ref().map_or(0, |u| u.len().div_ceil(13)))
            .sum::<usize>();
    let me = alloc(count as u64 * 32)?;

    let mut entries: Vec<[u8; 32]> = Vec::new();
    if root {
        let mut l = [b' '; 11];
        for (d, c) in l.iter_mut().zip(label.bytes()) {
            *d = c.to_ascii_uppercase();
        }
        entries.push(entry(&l, ATTR_VOLUME_ID, 0, 0, now));
    } else {
        entries.push(entry(b".          ", ATTR_DIRECTORY, me, 0, now));
        // ".." names the root as cluster 0.
        entries.push(entry(b"..         ", ATTR_DIRECTORY, parent, 0, now));
    }
    for ((_, node), (short, long)) in dir.entries.iter().zip(names) {
        let (attr, first, size) = match node {
            Node::File(data) => {
                let first = if data.size() == 0 {
                    0
                } else {
                    alloc(data.size())?
                };
                if data.size() != 0 {
                    writes.push((first, data.clone()));
                }
                (ATTR_ARCHIVE, first, data.size() as u32)
            }
            Node::Dir(d) => {
                let first = lay_out(
                    d,
                    if root { 0 } else { me },
                    false,
                    label,
                    now,
                    alloc,
                    writes,
                )?;
                (ATTR_DIRECTORY, first, 0)
            }
        };
        if let Some(long) = long {
            entries.extend(long_entries(&long, &short));
        }
        entries.push(entry(&short, attr, first, size, now));
    }
    writes.push((me, Data::Bytes(entries.concat())));
    Ok(me)
}

fn entry(name: &[u8; 11], attr: u8, cluster: u32, size: u32, (date, time): (u16, u16)) -> [u8; 32] {
    let mut e = [0u8; 32];
    e[0..11].copy_from_slice(name);
    e[11] = attr;
    e[14..16].copy_from_slice(&time.to_le_bytes());
    e[16..18].copy_from_slice(&date.to_le_bytes());
    e[18..20].copy_from_slice(&date.to_le_bytes());
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[22..24].copy_from_slice(&time.to_le_bytes());
    e[24..26].copy_from_slice(&date.to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    e
}

/// The 8.3 name for `name`, and its UTF-16 long name unless the short name holds it as it is. A
/// name that fits 8.3 keeps it, upper-cased (`BOOTx64.efi` is `BOOTX64.EFI`, which firmware
/// looking for the removable-media boot path finds); any other gets a `~1`-style one.
fn short_name(name: &str, taken: &[[u8; 11]]) -> Result<([u8; 11], Option<Vec<u16>>)> {
    ensure!(
        !name.is_empty() && name.len() <= 255 && name != "." && name != "..",
        "bad file name {name:?}"
    );
    let valid = |c: char| c.is_ascii_alphanumeric() || "$%'-_@~`!(){}^#&".contains(c);
    let (base, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i + 1..]),
        _ => (name, ""),
    };
    let long = || Some(name.encode_utf16().collect());
    let fits = base.len() <= 8
        && ext.len() <= 3
        && !base.is_empty()
        && base.chars().all(valid)
        && ext.chars().all(valid);
    if fits {
        let mut short = [b' '; 11];
        short[..base.len()].copy_from_slice(base.to_ascii_uppercase().as_bytes());
        short[8..8 + ext.len()].copy_from_slice(ext.to_ascii_uppercase().as_bytes());
        if !taken.contains(&short) {
            let upper = !name.chars().any(|c| c.is_ascii_lowercase());
            return Ok((short, if upper { None } else { long() }));
        }
    }
    let clean = |s: &str, n: usize| -> Vec<u8> {
        s.chars()
            .filter(|c| *c != ' ' && *c != '.')
            .map(|c| {
                if valid(c) {
                    c.to_ascii_uppercase() as u8
                } else {
                    b'_'
                }
            })
            .take(n)
            .collect()
    };
    let b = clean(base.trim_start_matches('.'), 6);
    let e = clean(ext, 3);
    for n in 1..1_000_000 {
        let tail = format!("~{n}");
        let keep = b.len().min(8 - tail.len());
        let mut s = [b' '; 11];
        s[..keep].copy_from_slice(&b[..keep]);
        s[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        s[8..8 + e.len()].copy_from_slice(&e);
        if !taken.contains(&s) {
            return Ok((s, long()));
        }
    }
    bail!("no short name for {name:?}")
}

fn checksum(short: &[u8; 11]) -> u8 {
    short
        .iter()
        .fold(0u8, |s, c| s.rotate_right(1).wrapping_add(*c))
}

/// The long-name entries for `long`, in the order they are stored: the last part first.
fn long_entries(long: &[u16], short: &[u8; 11]) -> Vec<[u8; 32]> {
    let sum = checksum(short);
    let parts = long.len().div_ceil(13);
    let mut out = Vec::new();
    for i in (0..parts).rev() {
        let mut chars = [0xffffu16; 13];
        for (j, slot) in chars.iter_mut().enumerate() {
            let k = i * 13 + j;
            if k < long.len() {
                *slot = long[k];
            } else if k == long.len() {
                *slot = 0;
            }
        }
        let mut e = [0u8; 32];
        e[0] = (i as u8 + 1) | if i + 1 == parts { 0x40 } else { 0 };
        e[11] = ATTR_LONG_NAME;
        e[13] = sum;
        for (j, c) in chars.iter().enumerate() {
            let at = match j {
                0..5 => 1 + j * 2,
                5..11 => 14 + (j - 5) * 2,
                _ => 28 + (j - 11) * 2,
            };
            e[at..at + 2].copy_from_slice(&c.to_le_bytes());
        }
        out.push(e);
    }
    out
}

fn boot_sector(
    total: u32,
    per_cluster: u32,
    fat_size: u32,
    hidden: u32,
    label: &str,
    serial: u32,
) -> [u8; SECTOR] {
    let mut b = [0u8; SECTOR];
    b[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    b[3..11].copy_from_slice(b"MSWIN4.1");
    b[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    b[13] = per_cluster as u8;
    b[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
    b[16] = FATS as u8;
    b[21] = 0xf8; // media: fixed disk
    b[24..26].copy_from_slice(&63u16.to_le_bytes());
    b[26..28].copy_from_slice(&255u16.to_le_bytes());
    b[28..32].copy_from_slice(&hidden.to_le_bytes());
    b[32..36].copy_from_slice(&total.to_le_bytes());
    b[36..40].copy_from_slice(&fat_size.to_le_bytes());
    b[44..48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
    b[48..50].copy_from_slice(&1u16.to_le_bytes()); // FSInfo sector
    b[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
    b[64] = 0x80;
    b[66] = 0x29;
    b[67..71].copy_from_slice(&serial.to_le_bytes());
    let mut l = [b' '; 11];
    for (d, c) in l.iter_mut().zip(label.bytes()) {
        *d = c.to_ascii_uppercase();
    }
    b[71..82].copy_from_slice(&l);
    b[82..90].copy_from_slice(b"FAT32   ");
    b[510] = 0x55;
    b[511] = 0xaa;
    b
}

fn fsinfo(free: u32, next_free: u32) -> [u8; SECTOR] {
    let mut b = [0u8; SECTOR];
    b[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    b[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    b[488..492].copy_from_slice(&free.to_le_bytes());
    b[492..496].copy_from_slice(&next_free.to_le_bytes());
    b[508..512].copy_from_slice(&0xaa55_0000u32.to_le_bytes());
    b
}

/// A UTC time as FAT's (date, time): days since 1980 and 2-second units.
pub fn dos_time(t: std::time::SystemTime) -> (u16, u16) {
    let secs = t
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let (y, m, d) = civil(secs / 86400);
    let s = secs % 86400;
    if y < 1980 {
        return (0x21, 0);
    }
    let date = (((y - 1980) as u16) << 9) | ((m as u16) << 5) | d as u16;
    let time =
        (((s / 3600) as u16) << 11) | ((((s / 60) % 60) as u16) << 5) | ((s % 60) / 2) as u16;
    (date, time)
}

/// Days since 1970-01-01 as (year, month, day), by Howard Hinnant's civil_from_days.
pub fn civil(days: u64) -> (i64, u32, u32) {
    let z = days as i64 + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

/// The files and folders of a volume `make` wrote, by path: for tests.
#[cfg(test)]
fn list(volume: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    let u16_at = |b: &[u8], o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as u32;
    let u32_at = |b: &[u8], o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let per_cluster = volume[13] as u32;
    let fat_size = u32_at(volume, 36);
    let data_start = RESERVED + FATS * fat_size;
    let fat = |c: u32| u32_at(volume, RESERVED as usize * SECTOR + c as usize * 4);
    let read = |first: u32, size: Option<u32>| -> Vec<u8> {
        let mut out = Vec::new();
        let mut c = first;
        while (2..0x0fff_fff8).contains(&c) {
            let at = (data_start + (c - 2) * per_cluster) as usize * SECTOR;
            out.extend_from_slice(&volume[at..at + per_cluster as usize * SECTOR]);
            c = fat(c);
        }
        if let Some(s) = size {
            out.truncate(s as usize);
        }
        out
    };
    let mut files = std::collections::BTreeMap::new();
    let mut stack = vec![(String::new(), 2u32)];
    while let Some((path, cluster)) = stack.pop() {
        let d = read(cluster, None);
        let mut long: Vec<u16> = Vec::new();
        for e in d.chunks(32) {
            if e[0] == 0 {
                break;
            }
            if e[11] == ATTR_LONG_NAME {
                let mut part = Vec::new();
                for at in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
                    let c = u16_at(e, at) as u16;
                    if c == 0 || c == 0xffff {
                        break;
                    }
                    part.push(c);
                }
                part.extend(long);
                long = part;
                continue;
            }
            if e[11] & ATTR_VOLUME_ID != 0 || e[0] == b'.' {
                long.clear();
                continue;
            }
            let name = if long.is_empty() {
                let b = String::from_utf8_lossy(&e[0..8]).trim_end().to_string();
                let x = String::from_utf8_lossy(&e[8..11]).trim_end().to_string();
                if x.is_empty() { b } else { format!("{b}.{x}") }
            } else {
                String::from_utf16_lossy(&long)
            };
            long.clear();
            let first = (u16_at(e, 20) << 16) | u16_at(e, 26);
            let p = format!("{path}/{name}");
            if e[11] & ATTR_DIRECTORY != 0 {
                files.insert(format!("{p}/"), Vec::new());
                stack.push((p, first));
            } else {
                files.insert(p, read(first, Some(u32_at(e, 28))));
            }
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn volume(size: u64, files: &[(&str, &[u8])]) -> Vec<u8> {
        let files: Vec<(String, Data)> = files
            .iter()
            .map(|(p, d)| (p.to_string(), Data::Bytes(d.to_vec())))
            .collect();
        let pieces = make(size, 2048, "OPENCORE", 1, &files, (0x5b4c, 0)).unwrap();
        super::super::Disk { size, pieces }.to_bytes().unwrap()
    }

    #[test]
    fn writes_files_that_read_back() {
        let big: Vec<u8> = (0..3000u32).map(|i| i as u8).collect();
        let vol = volume(
            40 << 20,
            &[
                ("EFI/BOOT/BOOTx64.efi", b"boot"),
                ("EFI/BOOT/.contentFlavour", b"OpenCore"),
                ("EFI/OC/config.plist", &big),
                ("EFI/OC/Kexts/", b""),
                ("README.TXT", b"hi"),
            ],
        );
        assert_eq!(vol[13], 1);
        let got = list(&vol);
        assert_eq!(got["/EFI/BOOT/BOOTx64.efi"], b"boot");
        assert_eq!(got["/EFI/BOOT/.contentFlavour"], b"OpenCore");
        assert_eq!(got["/EFI/OC/config.plist"], big);
        assert!(got.contains_key("/EFI/OC/Kexts/"));
        assert_eq!(got["/README.TXT"], b"hi");
        assert_eq!(&vol[510..512], &[0x55, 0xaa]);
        assert_eq!(&vol[82..90], b"FAT32   ");
    }

    #[test]
    fn larger_volumes_take_larger_clusters() {
        let big: Vec<u8> = (0..100_000u32).map(|i| (i % 7) as u8).collect();
        let vol = volume(
            300 << 20,
            &[
                ("com.apple.recovery.boot/BaseSystem.dmg", &big),
                ("com.apple.recovery.boot/BaseSystem.chunklist", b"CNKL"),
            ],
        );
        assert_eq!(vol[13], 8);
        let got = list(&vol);
        assert_eq!(got["/com.apple.recovery.boot/BaseSystem.dmg"], big);
        assert_eq!(
            got["/com.apple.recovery.boot/BaseSystem.chunklist"],
            b"CNKL"
        );
    }

    #[test]
    fn makes_short_names() {
        let (s, l) = short_name("EFI", &[]).unwrap();
        assert_eq!(&s, b"EFI        ");
        assert!(l.is_none());
        let (s, l) = short_name("OpenCore.efi", &[]).unwrap();
        assert_eq!(&s, b"OPENCOREEFI");
        assert!(l.is_some());
        let (s, _) = short_name("OpenRuntime.efi", &[]).unwrap();
        assert_eq!(&s, b"OPENRU~1EFI");
        let (s, l) = short_name("BOOTx64.efi", &[]).unwrap();
        assert_eq!(&s, b"BOOTX64 EFI");
        assert_eq!(String::from_utf16(&l.unwrap()).unwrap(), "BOOTx64.efi");
        let (s, _) = short_name(".contentVisibility", &[]).unwrap();
        assert_eq!(&s, b"CONTEN~1   ");
        let (s, _) = short_name(".contentVisibility", &[*b"CONTEN~1   "]).unwrap();
        assert_eq!(&s, b"CONTEN~2   ");
    }

    #[test]
    fn checksums_short_names() {
        // The specification's checksum of "OPENCO~1EFI", worked by hand-rotating each byte in.
        let mut sum = 0u8;
        for c in b"OPENCO~1EFI" {
            sum = (if sum & 1 != 0 { 0x80 } else { 0 }) + (sum >> 1);
            sum = sum.wrapping_add(*c);
        }
        assert_eq!(checksum(b"OPENCO~1EFI"), sum);
    }

    #[test]
    fn converts_dates() {
        assert_eq!(civil(0), (1970, 1, 1));
        assert_eq!(civil(20738), (2026, 10, 12));
        let t = std::time::UNIX_EPOCH + std::time::Duration::from_secs(20738 * 86400 + 3600 + 62);
        assert_eq!(
            dos_time(t),
            ((46 << 9) | (10 << 5) | 12, (1 << 11) | (1 << 5) | 1)
        );
    }
}
