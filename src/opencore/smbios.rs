//! The Mac a VM claims to be: its model, serial number, logic board serial (MLB), system UUID and
//! ROM, which OpenCore writes into SMBIOS and NVRAM.
//!
//! Serials follow the format Apple's are in, as macserial generates them (OpenCorePkg's
//! Utilities/macserial, BSD-3-Clause; Copyright (c) 2018-2020 vit9696, Copyright (c) 2020 Matis
//! Schotte), from its tables for the models below. Its FORMAT.md describes the fields:
//! - a 12-character serial: location (3), year (1), week (1), production line (3), model (4);
//! - a 17-character MLB: location (3), year (1 digit), week (2 digits), two random blocks, the
//!   board code (4), a random suffix (2), with a base-34 checksum over the whole.
//!
//! A serial made this way has the right shape for its model; whether Apple knows it is chance.

use anyhow::{Result, bail, ensure};

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Model {
    pub name: &'static str,
    /// The macOS versions it runs, for the interface.
    pub runs: &'static str,
    location: &'static str,
    years: &'static [u32],
    model_code: &'static str,
    board_code: &'static str,
}

/// iMac20,1 runs macOS 10.15.6 to 26: every x86-64 macOS a reims-vgpu guest runs. iMacPro1,1 runs
/// 10.13 to 15, for older guests.
pub const MODELS: &[Model] = &[
    Model {
        name: "iMac20,1",
        runs: "macOS 10.15.6 to 26",
        location: "C02",
        years: &[2020, 2021, 2022],
        model_code: "PN5T",
        board_code: "PHC1",
    },
    Model {
        name: "iMacPro1,1",
        runs: "macOS 10.13 to 15",
        location: "C02",
        years: &[2017, 2018, 2019, 2020, 2021],
        model_code: "HX87",
        board_code: "JG36",
    },
];

pub fn model(name: &str) -> Option<&'static Model> {
    MODELS
        .iter()
        .find(|m| m.name.eq_ignore_ascii_case(name.trim()))
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub struct Smbios {
    pub model: String,
    pub serial: String,
    pub mlb: String,
    pub uuid: String,
    /// Six bytes, as 12 hex digits.
    pub rom: String,
}

const BASE34: &[u8] = b"0123456789ABCDEFGHJKLMNPQRSTUVWXYZ";
const YEAR_CODES: &[u8] = b"CDFGHJKLMNPQRSTVWXYZ";
/// Week 1 to 53: the first 26 in the year's first half, the rest in its second, after a 0 that
/// has no week.
const WEEK_CODES: &[u8] = b"0123456789CDFGHJKLMNPQRTVWX123456789CDFGHJKLMNPQRTVWXY";
const MLB_BLOCK1: &[&str] = &[
    "200", "600", "403", "404", "405", "303", "108", "207", "609", "501", "306", "102", "701",
    "301", "501", "101", "300", "130", "100", "270", "310", "902", "104", "401", "902", "500",
    "700", "802",
];
const MLB_BLOCK2: &[&str] = &["GU", "4N", "J9", "QX", "OP", "CD", "GU"];
const MLB_BLOCK3: &[&str] = &[
    "1H", "1M", "AD", "1F", "A8", "UE", "JA", "JC", "8C", "CB", "FB",
];
/// The highest production line a first copy can encode (macserial's SERIAL_LINE_REPR_MAX).
const LINE_REPR_MAX: u32 = 1155;
const LINE_MAX: u32 = 3399;

pub struct Rng;

impl Rng {
    pub fn u32(&mut self) -> Result<u32> {
        getrandom::u32().map_err(|e| anyhow::anyhow!("random: {e}"))
    }
    pub fn below(&mut self, n: u32) -> Result<u32> {
        Ok(self.u32()? % n)
    }
    pub fn bytes(&mut self, b: &mut [u8]) -> Result<()> {
        getrandom::fill(b).map_err(|e| anyhow::anyhow!("random: {e}"))
    }
}

/// A new identity for `m`. The ROM is random, with the locally administered bit set, unless the
/// caller gives one (the VM's MAC address is the usual choice).
pub fn generate(m: &Model) -> Result<Smbios> {
    let mut rng = Rng;
    let year = m.years[rng.below(m.years.len() as u32)? as usize];
    let week = 1 + rng.below(52)?;
    let line = rng.below(LINE_MAX + 1)?;
    let serial = serial(m, year, week, line)?;
    let mlb = mlb(m, &serial, &mut rng)?;
    let mut uuid = [0u8; 16];
    rng.bytes(&mut uuid)?;
    let mut rom = [0u8; 6];
    rng.bytes(&mut rom)?;
    rom[0] = (rom[0] & 0xfe) | 0x02;
    Ok(Smbios {
        model: m.name.to_string(),
        serial,
        mlb,
        uuid: uuid_v4(uuid),
        rom: crate::http::hex(&rom).to_ascii_uppercase(),
    })
}

fn serial(m: &Model, year: u32, week: u32, line: u32) -> Result<String> {
    ensure!((2010..=2030).contains(&year), "year {year} out of range");
    ensure!((1..=53).contains(&week), "week {week} out of range");
    let base = if year >= 2020 { 2020 } else { 2010 };
    let year_code = YEAR_CODES[((year - base) * 2 + u32::from(week >= 27)) as usize];
    let week_code = WEEK_CODES[week as usize];
    // The line as three base-34 digits, weighted 68, 34 and 1, with the smallest first digit
    // that reaches it (its first copy).
    let rmin = if line > LINE_REPR_MAX {
        (line - LINE_REPR_MAX).div_ceil(68)
    } else {
        0
    };
    let rest = line - rmin * 68;
    let mut s = String::from(m.location);
    s.push(year_code as char);
    s.push(week_code as char);
    s.push(BASE34[rmin as usize] as char);
    s.push(BASE34[(rest / 34) as usize] as char);
    s.push(BASE34[(rest % 34) as usize] as char);
    s.push_str(m.model_code);
    Ok(s)
}

/// A new MLB for a serial number someone gave, made to go with it.
pub fn mlb_for(m: &Model, serial: &str) -> Result<String> {
    mlb(m, serial, &mut Rng)
}

/// The MLB for `serial`, made on the same week or just before, as macserial derives it.
fn mlb(m: &Model, serial: &str, rng: &mut Rng) -> Result<String> {
    let b = serial.as_bytes();
    ensure!(
        b.len() == 12,
        "a 12-character serial is needed for a 17-character MLB"
    );
    let (year_code, week_code) = (b[3], b[4]);
    let mut year = YEAR_CODES
        .iter()
        .position(|c| *c == year_code)
        .map(|i| (i / 2) as u32)
        .unwrap_or(0);
    let mut week = if b"DGJLNQSVXZ".contains(&year_code) {
        27
    } else {
        0
    };
    if let Some(i) = b"123456789CDFGHJKLMNPQRSTVWXYZ"
        .iter()
        .position(|c| *c == week_code)
    {
        week += i as u32 + 1;
    }
    ensure!(week >= 1, "serial {serial} has no week");
    week -= 1;
    if week == 0 {
        week = 53;
        year = if year == 0 { 9 } else { year - 1 };
    }
    for _ in 0..100_000 {
        let s = format!(
            "{}{}{:02}{}{}{}{}",
            m.location,
            year,
            week,
            MLB_BLOCK1[rng.below(MLB_BLOCK1.len() as u32)? as usize],
            MLB_BLOCK2[rng.below(MLB_BLOCK2.len() as u32)? as usize],
            m.board_code,
            MLB_BLOCK3[rng.below(MLB_BLOCK3.len() as u32)? as usize],
        );
        if mlb_checksum_ok(&s) {
            return Ok(s);
        }
    }
    bail!("no MLB with a valid checksum found")
}

/// The 17-character MLB checksum: base-34 digits weighted 3 and 1 alternately, summing to a
/// multiple of 34.
pub fn mlb_checksum_ok(mlb: &str) -> bool {
    let len = mlb.len();
    let mut sum = 0usize;
    for (i, c) in mlb.bytes().enumerate() {
        let Some(v) = BASE34.iter().position(|x| *x == c) else {
            return false;
        };
        sum += if (i & 1) == (len & 1) { 3 } else { 1 } * v;
    }
    sum.is_multiple_of(34)
}

fn uuid_v4(mut b: [u8; 16]) -> String {
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h = crate::http::hex(&b).to_ascii_uppercase();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

/// A ROM given as 12 hex digits or as a MAC address (`52:54:00:12:34:56`).
pub fn parse_rom(s: &str) -> Result<String> {
    let h: String = s
        .chars()
        .filter(|c| !matches!(c, ':' | '-' | ' '))
        .collect();
    ensure!(
        h.len() == 12 && h.chars().all(|c| c.is_ascii_hexdigit()),
        "a ROM is six bytes: 12 hex digits or a MAC address, not {s:?}"
    );
    Ok(h.to_ascii_uppercase())
}

pub fn parse_uuid(s: &str) -> Result<String> {
    let h: String = s.chars().filter(|c| *c != '-').collect();
    ensure!(
        h.len() == 32 && h.chars().all(|c| c.is_ascii_hexdigit()),
        "not a UUID: {s:?}"
    );
    let h = h.to_ascii_uppercase();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serial_fields_follow_the_format() {
        let m = model("iMac20,1").unwrap();
        // The second half of 2020 is D; week 30 there is 4 (30 - 26); line 1000 is 0, V (29), E (14).
        assert_eq!(serial(m, 2020, 30, 1000).unwrap(), "C02D40VEPN5T");
        // First half of 2017 is T, week 5; a line past the first copies' range moves the first
        // digit.
        let p = model("imacpro1,1").unwrap();
        assert_eq!(serial(p, 2017, 5, 3399).unwrap(), "C02T5ZZZHX87");
    }

    #[test]
    fn known_mlbs_pass_the_checksum() {
        // The MLB of macrecovery's selfcheck, and one macserial made for iMac20,1.
        assert!(mlb_checksum_ok("F5K105303J9K3F71M"));
        assert!(mlb_checksum_ok("C02051310QXPHC11F"));
        assert!(!mlb_checksum_ok("C02051310QXPHC11G"));
    }

    #[test]
    fn generated_identities_are_well_formed() {
        for m in MODELS {
            for _ in 0..50 {
                let s = generate(m).unwrap();
                assert_eq!(s.serial.len(), 12);
                assert!(s.serial.ends_with(m.model_code));
                assert_eq!(s.mlb.len(), 17);
                assert!(mlb_checksum_ok(&s.mlb));
                assert_eq!(&s.mlb[11..15], m.board_code);
                assert_eq!(s.uuid.len(), 36);
                assert_eq!(&s.uuid[14..15], "4");
                assert_eq!(s.rom.len(), 12);
                // An MLB made for a given serial is from the same week as the one made with it.
                let mlb = mlb_for(m, &s.serial).unwrap();
                assert!(mlb_checksum_ok(&mlb));
                assert_eq!(mlb[..6], s.mlb[..6]);
            }
        }
    }

    #[test]
    fn parses_roms_and_uuids() {
        assert_eq!(parse_rom("52:54:00:d7:83:02").unwrap(), "525400D78302");
        assert!(parse_rom("5254").is_err());
        assert_eq!(
            parse_uuid("ce36c47babd64d3585db37abe424cb5d").unwrap(),
            "CE36C47B-ABD6-4D35-85DB-37ABE424CB5D"
        );
    }
}
