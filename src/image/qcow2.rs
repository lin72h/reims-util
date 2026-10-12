//! A qcow2 image (version 3, as QEMU's docs/interop/qcow2.txt specifies it) of a disk held in
//! memory: only its clusters that are not all zero are stored, so a 128 MiB disk holding a few
//! MiB of files is a file of a few MiB.
//!
//! The layout, in 64 KiB clusters: the header; the refcount table; its refcount blocks; the L1
//! table; an L2 table for each 512 MiB of the disk that has data; the data clusters. Every
//! cluster is used once, so its refcount is 1 and every L1 and L2 entry carries QCOW_OFLAG_COPIED.

use std::io::Write;

const CLUSTER_BITS: u32 = 16;
const CLUSTER: u64 = 1 << CLUSTER_BITS;
/// 16-bit refcounts.
const REFCOUNT_ORDER: u32 = 4;
const L2_ENTRIES: u64 = CLUSTER / 8;
const REFCOUNTS_PER_BLOCK: u64 = CLUSTER * 8 / (1 << REFCOUNT_ORDER);
const COPIED: u64 = 1 << 63;
const HEADER_LENGTH: u32 = 104;

pub fn write(disk: &[u8], out: &mut impl Write) -> std::io::Result<()> {
    let size = disk.len() as u64;
    let clusters_in_disk = size.div_ceil(CLUSTER);
    let data: Vec<u64> = (0..clusters_in_disk)
        .filter(|&i| {
            let start = (i * CLUSTER) as usize;
            let end = (start + CLUSTER as usize).min(disk.len());
            disk[start..end].iter().any(|b| *b != 0)
        })
        .collect();
    let l1_entries = clusters_in_disk.div_ceil(L2_ENTRIES).max(1);
    let l1_clusters = (l1_entries * 8).div_ceil(CLUSTER);
    let mut l2_tables: Vec<u64> = data.iter().map(|c| c / L2_ENTRIES).collect();
    l2_tables.dedup();

    // The refcount blocks must cover every cluster, themselves included.
    let fixed = 1 + 1 + l1_clusters + l2_tables.len() as u64 + data.len() as u64;
    let mut refblocks = 1u64;
    while (fixed + refblocks).div_ceil(REFCOUNTS_PER_BLOCK) > refblocks {
        refblocks += 1;
    }
    // One refcount table cluster holds 8192 refcount blocks, for 16 TiB of clusters.
    if refblocks > CLUSTER / 8 {
        return Err(std::io::Error::other(
            "the disk is too large for a one-cluster refcount table",
        ));
    }
    let total = fixed + refblocks;

    let refcount_table_at = 1;
    let refblocks_at = 2;
    let l1_at = refblocks_at + refblocks;
    let l2_at = l1_at + l1_clusters;
    let data_at = l2_at + l2_tables.len() as u64;

    let mut header = vec![0u8; CLUSTER as usize];
    header[0..4].copy_from_slice(b"QFI\xfb");
    header[4..8].copy_from_slice(&3u32.to_be_bytes());
    header[20..24].copy_from_slice(&CLUSTER_BITS.to_be_bytes());
    header[24..32].copy_from_slice(&size.to_be_bytes());
    header[36..40].copy_from_slice(&(l1_entries as u32).to_be_bytes());
    header[40..48].copy_from_slice(&(l1_at * CLUSTER).to_be_bytes());
    header[48..56].copy_from_slice(&(refcount_table_at * CLUSTER).to_be_bytes());
    header[56..60].copy_from_slice(&1u32.to_be_bytes());
    header[96..100].copy_from_slice(&REFCOUNT_ORDER.to_be_bytes());
    header[100..104].copy_from_slice(&HEADER_LENGTH.to_be_bytes());
    // The header extensions end at once (type 0, length 0), already zero.
    out.write_all(&header)?;

    let mut table = vec![0u8; CLUSTER as usize];
    for i in 0..refblocks {
        let at = (i * 8) as usize;
        table[at..at + 8].copy_from_slice(&((refblocks_at + i) * CLUSTER).to_be_bytes());
    }
    out.write_all(&table)?;

    for b in 0..refblocks {
        let mut block = vec![0u8; CLUSTER as usize];
        for j in 0..REFCOUNTS_PER_BLOCK {
            let cluster = b * REFCOUNTS_PER_BLOCK + j;
            if cluster >= total {
                break;
            }
            let at = (j * 2) as usize;
            block[at..at + 2].copy_from_slice(&1u16.to_be_bytes());
        }
        out.write_all(&block)?;
    }

    let mut l1 = vec![0u8; (l1_clusters * CLUSTER) as usize];
    for (n, t) in l2_tables.iter().enumerate() {
        let at = (*t * 8) as usize;
        l1[at..at + 8].copy_from_slice(&(((l2_at + n as u64) * CLUSTER) | COPIED).to_be_bytes());
    }
    out.write_all(&l1)?;

    let mut next_data = data_at;
    for t in &l2_tables {
        let mut l2 = vec![0u8; CLUSTER as usize];
        for c in data.iter().filter(|c| *c / L2_ENTRIES == *t) {
            let at = ((c % L2_ENTRIES) * 8) as usize;
            l2[at..at + 8].copy_from_slice(&((next_data * CLUSTER) | COPIED).to_be_bytes());
            next_data += 1;
        }
        out.write_all(&l2)?;
    }

    for c in &data {
        let start = (c * CLUSTER) as usize;
        let end = (start + CLUSTER as usize).min(disk.len());
        out.write_all(&disk[start..end])?;
        if end - start < CLUSTER as usize {
            out.write_all(&vec![0u8; CLUSTER as usize - (end - start)])?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_cluster(img: &[u8], virt: u64) -> Option<&[u8]> {
        let be = |o: usize| u64::from_be_bytes(img[o..o + 8].try_into().unwrap());
        let l1_at = be(40) as usize;
        let l1 = be(l1_at + (virt / CLUSTER / L2_ENTRIES) as usize * 8) & !COPIED;
        if l1 == 0 {
            return None;
        }
        let l2 = be(l1 as usize + ((virt / CLUSTER) % L2_ENTRIES) as usize * 8) & !COPIED;
        if l2 == 0 {
            return None;
        }
        Some(&img[l2 as usize..l2 as usize + CLUSTER as usize])
    }

    #[test]
    fn stores_only_clusters_with_data() {
        let mut disk = vec![0u8; 3 * CLUSTER as usize + 512];
        disk[5] = 1;
        disk[3 * CLUSTER as usize + 10] = 2;
        let mut img = Vec::new();
        write(&disk, &mut img).unwrap();
        // Header, refcount table, one refcount block, L1, one L2, two data clusters.
        assert_eq!(img.len(), 7 * CLUSTER as usize);
        assert_eq!(read_cluster(&img, 0).unwrap()[5], 1);
        assert!(read_cluster(&img, CLUSTER).is_none());
        assert_eq!(read_cluster(&img, 3 * CLUSTER).unwrap()[10], 2);
        // Seven clusters, each counted once.
        let block = &img[2 * CLUSTER as usize..3 * CLUSTER as usize];
        let counts: Vec<u16> = block
            .chunks(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        assert!(counts[..7].iter().all(|c| *c == 1));
        assert!(counts[7..].iter().all(|c| *c == 0));
    }
}
