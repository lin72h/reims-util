//! Apple's chunklists (`CNKL`): the SHA-256 of each chunk of a recovery image or an installer
//! package, and a check of the list itself.
//!
//! The layout (little-endian), as macrecovery reads it (OpenCorePkg's Utilities/macrecovery,
//! BSD-3-Clause):
//! - a 0x24-byte header: magic `CNKL`, header size (0x24), file version 1, chunk method 1,
//!   signature method, a pad byte, the chunk count, the chunks' offset (0x24) and the signature's;
//! - each chunk: its size (u32) and SHA-256;
//! - the signature over the SHA-256 of the header and chunks: method 1 is a 2048-bit RSA
//!   signature (PKCS#1 v1.5) by Apple's EFI ROM key, method 2 the bare SHA-256.
//!
//! A recovery image's chunklist comes over plain HTTP from osrecovery.apple.com, so it must carry
//! the signature: that makes the image's every byte Apple's. An installer's comes over HTTPS
//! from the software update catalog (also HTTPS), with method 2.

use crate::http::Check;
use crate::progress::{self, Reporter};
use anyhow::{Result, bail, ensure};
use num_bigint::BigUint;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

const HEADER_SIZE: usize = 0x24;
const CHUNK_SIZE: usize = 0x24;

/// Apple's EFI ROM public key 1, the modulus of the RSA key that signs recovery chunklists (its
/// exponent is 65537).
const APPLE_EFI_ROM_KEY_1: &str = "C3E748CAD9CD384329E10E25A91E43E1A762FF529ADE578C935BDDF9B13F2179\
D4855E6FC89E9E29CA12517D17DFA1EDCE0BEBF0EA7B461FFE61D94E2BDF72C196F89ACD3536B644064014DAE25A15DB\
6BB0852ECBD120916318D1CCDEA3C84C92ED743FC176D0BACA920D3FCF3158AFF731F88CE0623182A8ED67E650515F7\
5745909F07D415F55FC15A35654D118C55A462D37A3ACDA08612F3F3F6571761EFCCBCC299AEE99B3A4FD6212CCFFF5E\
F37A2C334E871191F7E1C31960E010A54E86FA3F62E6D6905E1CD57732410A3EB0C6B4DEFDABE9F59BF1618758C751C\
D56CEF851D1C0EAA1C558E37AC108DA9089863D20E2E7E4BF475EC66FE6B3EFDCF";

/// PKCS#1 v1.5's DigestInfo prefix for SHA-256.
const SHA256_DIGEST_INFO: [u8; 19] = [
    0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02, 0x01, 0x05,
    0x00, 0x04, 0x20,
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Signature {
    /// Signed by Apple's EFI ROM key, checked.
    Rsa,
    /// The bare digest of the list, checked.
    Digest,
}

#[derive(Clone, Debug)]
pub struct Chunklist {
    pub chunks: Vec<(u32, [u8; 32])>,
    pub signature: Signature,
}

impl Chunklist {
    /// Parses a chunklist and checks its signature or digest.
    pub fn parse(data: &[u8]) -> Result<Chunklist> {
        ensure!(data.len() >= HEADER_SIZE, "the chunklist is too short");
        ensure!(&data[0..4] == b"CNKL", "not a chunklist");
        let u32_at =
            |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
        let u64_at = |o: usize| {
            let mut b = [0u8; 8];
            b.copy_from_slice(&data[o..o + 8]);
            u64::from_le_bytes(b)
        };
        ensure!(
            u32_at(4) as usize == HEADER_SIZE,
            "unknown chunklist header size"
        );
        ensure!(data[8] == 1 && data[9] == 1, "unknown chunklist version");
        let method = data[10];
        let count = u64_at(12);
        let chunk_offset = u64_at(20);
        let sig_offset = u64_at(28);
        ensure!(count > 0 && count < 1 << 24, "chunklist has {count} chunks");
        ensure!(
            chunk_offset as usize == HEADER_SIZE,
            "unknown chunklist layout"
        );
        let chunks_end = HEADER_SIZE + count as usize * CHUNK_SIZE;
        ensure!(
            sig_offset as usize == chunks_end,
            "unknown chunklist layout"
        );
        ensure!(data.len() >= chunks_end, "the chunklist is cut short");
        let digest: [u8; 32] = Sha256::digest(&data[..chunks_end]).into();
        let sig = &data[chunks_end..];
        let signature = match method {
            1 => {
                ensure!(
                    sig.len() == 256,
                    "the chunklist's signature is {} bytes",
                    sig.len()
                );
                ensure!(
                    rsa_verify(APPLE_EFI_ROM_KEY_1, sig, &digest),
                    "the chunklist's signature is not Apple's"
                );
                Signature::Rsa
            }
            2 => {
                ensure!(
                    sig.len() == 32 && sig == digest,
                    "the chunklist's digest does not match"
                );
                Signature::Digest
            }
            m => bail!("unknown chunklist signature method {m}"),
        };
        let chunks = data[HEADER_SIZE..chunks_end]
            .as_chunks::<CHUNK_SIZE>()
            .0
            .iter()
            .map(|c| {
                let size = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                let mut h = [0u8; 32];
                h.copy_from_slice(&c[4..36]);
                (size, h)
            })
            .collect();
        Ok(Chunklist { chunks, signature })
    }

    pub fn total(&self) -> u64 {
        self.chunks.iter().map(|(s, _)| *s as u64).sum()
    }
}

/// Whether `sig` (little-endian, as chunklists store it) is the PKCS#1 v1.5 signature of
/// `digest` by the 2048-bit key of modulus `modulus` (hex) and exponent 65537.
fn rsa_verify(modulus: &str, sig: &[u8], digest: &[u8; 32]) -> bool {
    let Some(n) = BigUint::parse_bytes(modulus.as_bytes(), 16) else {
        return false;
    };
    let s = BigUint::from_bytes_le(sig);
    if s >= n {
        return false;
    }
    let m = s.modpow(&BigUint::from(65537u32), &n).to_bytes_be();
    // The encoded message is 256 bytes, 00 01 FF..FF 00 DigestInfo digest; as a number its
    // leading zero byte drops.
    let mut expected = vec![0x01u8];
    expected.resize(256 - 1 - 1 - SHA256_DIGEST_INFO.len() - 32, 0xff);
    expected.push(0x00);
    expected.extend_from_slice(&SHA256_DIGEST_INFO);
    expected.extend_from_slice(digest);
    m == expected
}

/// Checks a download against its chunklist as it arrives.
pub struct ChunkCheck {
    list: Chunklist,
    /// The chunk being filled, and the bytes of it seen so far.
    index: usize,
    filled: u64,
    hasher: Sha256,
    /// Where the chunk being filled starts in the file.
    chunk_start: u64,
}

impl ChunkCheck {
    pub fn new(list: Chunklist) -> Self {
        ChunkCheck {
            list,
            index: 0,
            filled: 0,
            hasher: Sha256::new(),
            chunk_start: 0,
        }
    }

    pub fn total(&self) -> u64 {
        self.list.total()
    }
}

impl Check for ChunkCheck {
    fn resume(&mut self, file: &mut File, offset: u64, len: u64, r: &dyn Reporter) -> Result<u64> {
        file.seek(SeekFrom::Start(offset))?;
        let mut good = 0u64;
        let mut buf = Vec::new();
        for (size, hash) in &self.list.chunks {
            progress::check(r)?;
            let size = *size as u64;
            if good + size > len {
                break;
            }
            buf.resize(size as usize, 0);
            file.read_exact(&mut buf)?;
            if Sha256::digest(&buf)[..] != hash[..] {
                break;
            }
            // Said once there is something to check: an empty image's first chunk fails.
            if good == 0 {
                r.step("Checking what is already downloaded");
            }
            good += size;
            r.progress(good, Some(len));
        }
        Ok(good)
    }

    fn verifies(&self) -> bool {
        true
    }

    fn feed(&mut self, mut data: &[u8]) -> std::result::Result<(), u64> {
        while !data.is_empty() {
            let Some((size, hash)) = self.list.chunks.get(self.index) else {
                // More bytes than the chunklist lists.
                return Err(self.chunk_start);
            };
            let take = ((*size as u64 - self.filled) as usize).min(data.len());
            self.hasher.update(&data[..take]);
            self.filled += take as u64;
            data = &data[take..];
            if self.filled == *size as u64 {
                let h = std::mem::take(&mut self.hasher).finalize();
                if h[..] != hash[..] {
                    let start = self.chunk_start;
                    self.restart_at(start);
                    return Err(start);
                }
                self.chunk_start += *size as u64;
                self.index += 1;
                self.filled = 0;
            }
        }
        Ok(())
    }

    fn restart_at(&mut self, offset: u64) {
        // `offset` is always a chunk boundary: 0, or one resume or feed returned.
        let mut start = 0u64;
        let mut index = 0;
        for (size, _) in &self.list.chunks {
            if start + *size as u64 > offset {
                break;
            }
            start += *size as u64;
            index += 1;
        }
        self.index = index;
        self.chunk_start = start;
        self.filled = 0;
        self.hasher = Sha256::new();
    }

    fn finish(&mut self) -> Result<()> {
        ensure!(
            self.index == self.list.chunks.len() && self.filled == 0,
            "the download ended at chunk {} of {}",
            self.index,
            self.list.chunks.len()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A method-2 chunklist of the given chunks.
    fn make(chunks: &[&[u8]]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(b"CNKL");
        v.extend_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        v.extend_from_slice(&[1, 1, 2, 0]);
        v.extend_from_slice(&(chunks.len() as u64).to_le_bytes());
        v.extend_from_slice(&(HEADER_SIZE as u64).to_le_bytes());
        v.extend_from_slice(&((HEADER_SIZE + chunks.len() * CHUNK_SIZE) as u64).to_le_bytes());
        for c in chunks {
            v.extend_from_slice(&(c.len() as u32).to_le_bytes());
            v.extend_from_slice(&Sha256::digest(c));
        }
        let d = Sha256::digest(&v);
        v.extend_from_slice(&d);
        v
    }

    #[test]
    fn digest_chunklist_checks_chunks_across_feeds() {
        let a = vec![1u8; 100];
        let b = vec![2u8; 50];
        let list = Chunklist::parse(&make(&[&a, &b])).unwrap();
        assert_eq!(list.signature, Signature::Digest);
        assert_eq!(list.total(), 150);
        let mut c = ChunkCheck::new(list.clone());
        let mut all = a.clone();
        all.extend_from_slice(&b);
        for piece in all.chunks(7) {
            c.feed(piece).unwrap();
        }
        c.finish().unwrap();

        // A damaged second chunk sends the download back to its start.
        let mut c = ChunkCheck::new(list);
        all[120] ^= 1;
        assert_eq!(c.feed(&all), Err(100));
        c.feed(&b).unwrap();
        c.finish().unwrap();
    }

    #[test]
    fn rejects_a_tampered_list() {
        let mut v = make(&[b"abc"]);
        v[40] ^= 1;
        assert!(Chunklist::parse(&v).is_err());
    }

    #[test]
    fn checks_pkcs1_signatures() {
        // A throwaway key's signature of SHA-256("reims-util test"), made with `openssl pkeyutl
        // -sign -pkeyopt digest:sha256`, its bytes reversed as a chunklist stores them.
        const N: &str = "A4DEB083EE044B5441BAE670FD538C8E998FFEDE4913847A76A2B8C4D41C579E7F23A07DC4C9B04A08241710E856AE6F\
326AFC90756F1924B05854EF415D876BA8E593FB83CD1CF62A0FC597EB27E199669C3790E17044688D09E0CBE9FE4EE2\
FB5D66357BD89EEAD01FA8ED5D8E5D659C25E44F565CD133321355BD8E500332426BF2902C8D38063CD3520B2D280727\
6E140EE16FA1CCC00FFEEB041149698D1CD85C782874F7A22DFED6D0D4210EA2E7F84199CB72360A95666D5B460397A3\
07BEF86F38EA2A09E353B1F5B93DE25D2C31A6A16A13102CFF849CC970C6BDE3760CEA3F36FC163455293A8F4B4DC8CA\
49B26EB73B72F2BE1FEC8EBDDE869CDF";
        const S: &str = "1c1263853f9f2f87782f0acc863d8b6b9980ac4a9178844c6cb30010910b9ff175c2bd4dd2d95ee2f5d950c768cf7235\
71b4c4f6ea9e62831c915361cde68bbfb0d26a96bef07c93e16f31c88a2acf141b613492e0bd72d5e89cedfbbc5c3f0f\
6635a77439ea836fdb12ae5bb04f3392f8dcae195c4331c3726068109828a74ce80b09dcb87f8973c6514ef145201dcc\
7dc943625c8bef3e99f4e9990a8f1d3848186df2ba74e19df8bec3ab60244d07c3a133ebe7fce3490e7928a794c43e90\
fdfdadc37efae0fc45efaf4cb5f836c0db55644d1d9ee1327f7a0e71065bd64ec31d163f01b040624fff0fb2158a47cc\
8d44e901c022bcf493c33875197b4682";
        let sig: Vec<u8> = (0..S.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&S[i..i + 2], 16).unwrap())
            .collect();
        let digest: [u8; 32] = Sha256::digest(b"reims-util test").into();
        assert!(rsa_verify(N, &sig, &digest));
        let mut other = digest;
        other[0] ^= 1;
        assert!(!rsa_verify(N, &sig, &other));
        assert!(!rsa_verify(APPLE_EFI_ROM_KEY_1, &sig, &digest));
    }
}
