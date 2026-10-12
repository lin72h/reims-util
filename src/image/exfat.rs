//! An exFAT volume holding files in its root folder, as Microsoft's exFAT File System
//! Specification lays it out:
//! - the main boot region, sectors 0 to 11: the boot sector, eight extended boot sectors, the
//!   OEM parameters, a reserved sector, and the boot checksum; its backup, sectors 12 to 23;
//! - one FAT;
//! - the cluster heap, from cluster 2: the allocation bitmap, the up-case table, the root
//!   folder, then each file in consecutive clusters, with its FAT chain written.
//!
//! exFAT is here for what FAT32 cannot hold: a file of 4 GiB or more, such as a macOS installer
//! package (12 to 18 GB). macOS reads it, macOS Recovery included.

use super::Data;
use anyhow::{Result, ensure};

const SECTOR: u64 = 512;
/// Where the FAT starts: past both boot regions, 64 KiB in.
const FAT_OFFSET: u64 = 128;
const EOC: u32 = 0xffff_ffff;
const ENTRY: usize = 32;

/// A volume of `size` bytes (a whole partition) holding `files` in its root folder, as byte
/// ranges from the volume's start. `partition_offset` is the sector the partition starts at on
/// its disk; `label` is up to 11 characters; `serial` the volume serial number; `now` a FAT
/// (date, time), which every file is given.
pub fn make(
    size: u64,
    partition_offset: u64,
    label: &str,
    serial: u32,
    files: &[(String, Data)],
    now: (u16, u16),
) -> Result<Vec<(u64, Data)>> {
    ensure!(
        size.is_multiple_of(SECTOR) && size >= 1 << 20,
        "an exFAT volume is 1 MiB at least, in whole sectors"
    );
    let label: Vec<u16> = label.encode_utf16().collect();
    ensure!(label.len() <= 11, "an exFAT label is 11 characters at most");
    let volume_sectors = size / SECTOR;
    let shift = cluster_shift(size);
    let per_cluster = 1u64 << shift;
    let cluster_bytes = per_cluster * SECTOR;

    // The FAT is sized for every cluster the volume could have; the heap starts on a cluster
    // boundary after it.
    let fat_len = ((volume_sectors / per_cluster + 2) * 4).div_ceil(SECTOR);
    let heap = (FAT_OFFSET + fat_len).next_multiple_of(per_cluster);
    ensure!(heap < volume_sectors, "the volume is too small");
    let clusters = u32::try_from((volume_sectors - heap) / per_cluster)?;
    ensure!(clusters <= 0xffff_fff5, "the volume has too many clusters");

    let mut fat = vec![0u32; clusters as usize + 2];
    fat[0] = 0xffff_fff8;
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

    let bitmap_len = u64::from(clusters).div_ceil(8);
    let bitmap_at = alloc(bitmap_len)?;
    let upcase = upcase_table();
    let upcase_at = alloc(upcase.len() as u64)?;
    let upcase_map = expand_upcase(&upcase);

    let mut entries: Vec<[u8; ENTRY]> = Vec::new();
    let mut e = [0u8; ENTRY];
    e[0] = 0x83;
    e[1] = label.len() as u8;
    for (i, c) in label.iter().enumerate() {
        e[2 + i * 2..4 + i * 2].copy_from_slice(&c.to_le_bytes());
    }
    entries.push(e);
    let mut e = [0u8; ENTRY];
    e[0] = 0x81;
    e[20..24].copy_from_slice(&bitmap_at.to_le_bytes());
    e[24..32].copy_from_slice(&bitmap_len.to_le_bytes());
    entries.push(e);
    let mut e = [0u8; ENTRY];
    e[0] = 0x82;
    e[4..8].copy_from_slice(&checksum32(&upcase).to_le_bytes());
    e[20..24].copy_from_slice(&upcase_at.to_le_bytes());
    e[24..32].copy_from_slice(&(upcase.len() as u64).to_le_bytes());
    entries.push(e);

    let names: Vec<Vec<u16>> = files
        .iter()
        .map(|(name, _)| name.encode_utf16().collect())
        .collect();
    for ((name, _), units) in files.iter().zip(&names) {
        ensure!(
            !units.is_empty()
                && units.len() <= 255
                && !name.chars().any(|c| c < ' ' || "\"*/:<>?\\|".contains(c)),
            "bad exFAT file name {name:?}"
        );
    }
    let set_entries: usize = names.iter().map(|n| 2 + n.len().div_ceil(15)).sum();
    // One entry more, unused, ends the folder.
    let root_at = alloc(((entries.len() + set_entries + 1) * ENTRY) as u64)?;

    let mut pieces = Vec::new();
    let stamp = (u32::from(now.0) << 16) | u32::from(now.1);
    for ((_, data), name) in files.iter().zip(&names) {
        let first = if data.size() == 0 {
            0
        } else {
            alloc(data.size())?
        };
        if data.size() != 0 {
            pieces.push((first, data.clone()));
        }
        entries.extend(file_set(name, &upcase_map, first, data.size(), stamp));
    }
    let used = next - 2;

    let mut bitmap = vec![0u8; bitmap_len as usize];
    for i in 0..used as usize {
        bitmap[i / 8] |= 1 << (i % 8);
    }

    let boot = boot_region(&BootSector {
        partition_offset,
        volume_sectors,
        fat_len: fat_len as u32,
        heap: heap as u32,
        clusters,
        root: root_at,
        serial,
        shift,
        percent_in_use: (u64::from(used) * 100 / u64::from(clusters)) as u8,
    });
    let cluster_at = |c: u32| (heap + u64::from(c - 2) * per_cluster) * SECTOR;
    // Each structure fills its sectors or clusters, so none keeps what an earlier image left.
    let whole = |mut b: Vec<u8>, unit: u64| {
        b.resize(b.len().next_multiple_of(unit as usize), 0);
        Data::Bytes(b)
    };
    let mut out = vec![
        (0, Data::Bytes(boot.clone())),
        (12 * SECTOR, Data::Bytes(boot)),
        (
            FAT_OFFSET * SECTOR,
            whole(
                fat.iter().flat_map(|v| v.to_le_bytes()).collect(),
                fat_len * SECTOR,
            ),
        ),
        (cluster_at(bitmap_at), whole(bitmap, cluster_bytes)),
        (cluster_at(upcase_at), whole(upcase, cluster_bytes)),
        (cluster_at(root_at), whole(entries.concat(), cluster_bytes)),
    ];
    out.extend(pieces.into_iter().map(|(c, d)| {
        let d = match d {
            Data::Bytes(b) => whole(b, cluster_bytes),
            reserved => reserved,
        };
        (cluster_at(c), d)
    }));
    Ok(out)
}

/// Sectors per cluster, as a power of two, by Microsoft's defaults for the volume's size: 4 KiB
/// clusters up to 256 MiB, 32 KiB up to 32 GiB, 128 KiB above.
fn cluster_shift(size: u64) -> u8 {
    if size <= 256 << 20 {
        3
    } else if size <= 32 << 30 {
        6
    } else {
        8
    }
}

/// A file's directory entry set: the file entry, its stream extension, and its name in entries
/// of 15 characters.
fn file_set(name: &[u16], upcase: &[u16], first: u32, len: u64, stamp: u32) -> Vec<[u8; ENTRY]> {
    let name_entries = name.len().div_ceil(15);
    let mut set = Vec::with_capacity(2 + name_entries);
    let mut e = [0u8; ENTRY];
    e[0] = 0x85;
    e[1] = (1 + name_entries) as u8;
    e[4..6].copy_from_slice(&0x20u16.to_le_bytes()); // archive
    for at in [8, 12, 16] {
        e[at..at + 4].copy_from_slice(&stamp.to_le_bytes());
    }
    // The times are UTC: a valid offset of 0.
    e[22..25].fill(0x80);
    set.push(e);
    let mut e = [0u8; ENTRY];
    e[0] = 0xc0;
    e[1] = 0x01; // allocation possible; the FAT holds the chain
    e[3] = name.len() as u8;
    let hashed: Vec<u16> = name
        .iter()
        .map(|&c| upcase.get(c as usize).copied().unwrap_or(c))
        .collect();
    e[4..6].copy_from_slice(&name_hash(&hashed).to_le_bytes());
    e[8..16].copy_from_slice(&len.to_le_bytes());
    e[20..24].copy_from_slice(&first.to_le_bytes());
    e[24..32].copy_from_slice(&len.to_le_bytes());
    set.push(e);
    for part in name.chunks(15) {
        let mut e = [0u8; ENTRY];
        e[0] = 0xc1;
        for (i, c) in part.iter().enumerate() {
            e[2 + i * 2..4 + i * 2].copy_from_slice(&c.to_le_bytes());
        }
        set.push(e);
    }
    let sum = set_checksum(&set.concat());
    set[0][2..4].copy_from_slice(&sum.to_le_bytes());
    set
}

struct BootSector {
    partition_offset: u64,
    volume_sectors: u64,
    fat_len: u32,
    heap: u32,
    clusters: u32,
    root: u32,
    serial: u32,
    shift: u8,
    percent_in_use: u8,
}

/// The 12 sectors of a boot region: the boot sector, the extended boot sectors, the OEM
/// parameters and the reserved sector (both empty), and the checksum of the 11 before it.
fn boot_region(b: &BootSector) -> Vec<u8> {
    let mut r = vec![0u8; 12 * SECTOR as usize];
    let s = &mut r[..SECTOR as usize];
    s[0..3].copy_from_slice(&[0xeb, 0x76, 0x90]);
    s[3..11].copy_from_slice(b"EXFAT   ");
    s[64..72].copy_from_slice(&b.partition_offset.to_le_bytes());
    s[72..80].copy_from_slice(&b.volume_sectors.to_le_bytes());
    s[80..84].copy_from_slice(&(FAT_OFFSET as u32).to_le_bytes());
    s[84..88].copy_from_slice(&b.fat_len.to_le_bytes());
    s[88..92].copy_from_slice(&b.heap.to_le_bytes());
    s[92..96].copy_from_slice(&b.clusters.to_le_bytes());
    s[96..100].copy_from_slice(&b.root.to_le_bytes());
    s[100..104].copy_from_slice(&b.serial.to_le_bytes());
    s[104..106].copy_from_slice(&[0x00, 0x01]); // revision 1.00
    s[108] = 9; // 512-byte sectors
    s[109] = b.shift;
    s[110] = 1; // one FAT
    s[111] = 0x80;
    s[112] = b.percent_in_use;
    // No boot code: halt.
    s[120..510].fill(0xf4);
    s[510] = 0x55;
    s[511] = 0xaa;
    for i in 1..=8 {
        let end = (i + 1) * SECTOR as usize;
        r[end - 2] = 0x55;
        r[end - 1] = 0xaa;
    }
    let sum = boot_checksum(&r[..11 * SECTOR as usize]);
    for c in r[11 * SECTOR as usize..].chunks_mut(4) {
        c.copy_from_slice(&sum.to_le_bytes());
    }
    r
}

/// The boot checksum, which leaves out the boot sector's volume flags and percent in use.
fn boot_checksum(sectors: &[u8]) -> u32 {
    sectors
        .iter()
        .enumerate()
        .filter(|(i, _)| !matches!(i, 106 | 107 | 112))
        .fold(0u32, |s, (_, b)| {
            s.rotate_right(1).wrapping_add(u32::from(*b))
        })
}

/// An entry set's checksum, which leaves out the checksum's own two bytes.
fn set_checksum(set: &[u8]) -> u16 {
    set.iter()
        .enumerate()
        .filter(|(i, _)| !matches!(i, 2 | 3))
        .fold(0u16, |s, (_, b)| {
            s.rotate_right(1).wrapping_add(u16::from(*b))
        })
}

/// The hash of an up-cased name, over its UTF-16 code units' bytes.
fn name_hash(upcased: &[u16]) -> u16 {
    upcased
        .iter()
        .flat_map(|c| c.to_le_bytes())
        .fold(0u16, |s, b| s.rotate_right(1).wrapping_add(u16::from(b)))
}

/// The up-case table's checksum.
fn checksum32(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |s, b| s.rotate_right(1).wrapping_add(u32::from(*b)))
}

/// The up-case table, compressed as the specification allows: each code unit's upper case in
/// turn, with a run of ones that map to themselves written as 0xFFFF and the run's length. The
/// mapping is Unicode's simple upper case of each BMP character.
fn upcase_table() -> Vec<u8> {
    let up = |c: u32| -> u32 {
        let Some(ch) = char::from_u32(c) else {
            return c;
        };
        let mut u = ch.to_uppercase();
        match (u.next(), u.next()) {
            (Some(x), None) if (x as u32) < 0x1_0000 => x as u32,
            _ => c,
        }
    };
    let mut table: Vec<u16> = Vec::new();
    let mut c = 0u32;
    while c < 0x1_0000 {
        if up(c) != c {
            table.push(up(c) as u16);
            c += 1;
            continue;
        }
        let start = c;
        while c < 0x1_0000 && up(c) == c {
            c += 1;
        }
        // A run that reaches 0xFFFF is always compressed, so 0xFFFF is never a mapping.
        if c - start > 2 || c == 0x1_0000 {
            table.push(0xffff);
            table.push((c - start) as u16);
        } else {
            table.extend((start..c).map(|k| k as u16));
        }
    }
    table.iter().flat_map(|u| u.to_le_bytes()).collect()
}

/// A compressed up-case table as a code unit's upper case, by index.
fn expand_upcase(table: &[u8]) -> Vec<u16> {
    let units: Vec<u16> = table
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| u16::from_le_bytes(*b))
        .collect();
    let mut map = Vec::with_capacity(0x1_0000);
    let mut i = 0;
    while i < units.len() {
        if units[i] == 0xffff && i + 1 < units.len() {
            let n = units[i + 1];
            for _ in 0..n {
                map.push(map.len() as u16);
            }
            i += 2;
        } else {
            map.push(units[i]);
            i += 1;
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The files in the root folder of a volume `make` wrote, read back through its structures.
    fn list(vol: &[u8]) -> Vec<(String, Vec<u8>)> {
        let u32_at = |o: usize| u32::from_le_bytes(vol[o..o + 4].try_into().unwrap());
        let fat_at = u32_at(80) as usize * 512;
        let heap = u32_at(88) as usize * 512;
        let cluster = 512usize << vol[109];
        let chain = |first: u32, len: usize| -> Vec<u8> {
            let mut out = Vec::new();
            let mut c = first;
            while (2..0xffff_fff7).contains(&c) {
                let at = heap + (c as usize - 2) * cluster;
                out.extend_from_slice(&vol[at..at + cluster]);
                c = u32_at(fat_at + c as usize * 4);
            }
            out.truncate(len);
            out
        };
        let root = chain(u32_at(96), usize::MAX);
        let mut files = Vec::new();
        let mut i = 0;
        while root[i * 32] != 0 {
            let e = &root[i * 32..i * 32 + 32];
            if e[0] != 0x85 {
                i += 1;
                continue;
            }
            let n = e[1] as usize;
            let set = &root[i * 32..(i + 1 + n) * 32];
            assert_eq!(
                set_checksum(set),
                u16::from_le_bytes([e[2], e[3]]),
                "the set checksum"
            );
            let s = &set[32..64];
            let name_len = s[3] as usize;
            let mut name: Vec<u16> = Vec::new();
            for k in 0..n - 1 {
                let f = &set[64 + k * 32..96 + k * 32];
                for j in 0..15 {
                    name.push(u16::from_le_bytes([f[2 + j * 2], f[3 + j * 2]]));
                }
            }
            name.truncate(name_len);
            let len = u64::from_le_bytes(s[24..32].try_into().unwrap()) as usize;
            let first = u32::from_le_bytes(s[20..24].try_into().unwrap());
            files.push((String::from_utf16(&name).unwrap(), chain(first, len)));
            i += 1 + n;
        }
        files
    }

    fn volume(size: u64, files: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let files: Vec<(String, Data)> = files
            .iter()
            .map(|(n, d)| (n.to_string(), Data::Bytes(d.clone())))
            .collect();
        let pieces = make(size, 2048, "Installer", 7, &files, (0x5b4c, 0)).unwrap();
        super::super::Disk { size, pieces }.to_bytes().unwrap()
    }

    #[test]
    fn writes_files_that_read_back() {
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 253) as u8).collect();
        let vol = volume(
            64 << 20,
            &[
                ("InstallAssistant.pkg", big.clone()),
                ("install-macos.sh", b"#!/bin/sh\n".to_vec()),
                ("empty", Vec::new()),
            ],
        );
        let got = list(&vol);
        assert_eq!(got[0], ("InstallAssistant.pkg".to_string(), big));
        assert_eq!(got[1].1, b"#!/bin/sh\n");
        assert_eq!(got[2], ("empty".to_string(), Vec::new()));
        // The backup boot region is the main one.
        assert_eq!(vol[..12 * 512], vol[12 * 512..24 * 512]);
        assert_eq!(&vol[64..72], &2048u64.to_le_bytes());
    }

    #[test]
    fn boot_checksum_covers_the_region() {
        let vol = volume(64 << 20, &[("a", vec![1])]);
        let sum = boot_checksum(&vol[..11 * 512]);
        for c in vol[11 * 512..12 * 512].chunks(4) {
            assert_eq!(c, sum.to_le_bytes());
        }
        // Volume flags and percent in use are left out.
        let mut v = vol[..11 * 512].to_vec();
        v[106] ^= 1;
        v[112] ^= 0x7f;
        assert_eq!(boot_checksum(&v), sum);
    }

    #[test]
    fn upcase_table_maps_letters() {
        let map = expand_upcase(&upcase_table());
        assert_eq!(map.len(), 0x1_0000);
        assert_eq!(map['a' as usize], 'A' as u16);
        assert_eq!(map['z' as usize], 'Z' as u16);
        assert_eq!(map['A' as usize], 'A' as u16);
        assert_eq!(map['é' as usize], 'É' as u16);
        assert_eq!(map['1' as usize], '1' as u16);
        assert_eq!(map[0xffff], 0xffff);
    }

    #[test]
    fn hashes_names_as_the_specification_does() {
        // NameHash = ((NameHash & 1) ? 0x8000 : 0) + (NameHash >> 1) + byte, over each byte.
        let mut h = 0u16;
        for b in "ABC".encode_utf16().flat_map(|c| c.to_le_bytes()) {
            h = (if h & 1 != 0 { 0x8000 } else { 0 }) + (h >> 1) + u16::from(b);
        }
        assert_eq!(name_hash(&"ABC".encode_utf16().collect::<Vec<_>>()), h);
    }

    #[test]
    fn picks_cluster_sizes() {
        assert_eq!(cluster_shift(64 << 20), 3);
        assert_eq!(cluster_shift(18 << 30), 6);
        assert_eq!(cluster_shift(40 << 30), 8);
    }
}
