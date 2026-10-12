//! The one ACPI table the OpenCore image adds, `SSDT-EC-USBX.aml`, encoded here in AML.
//!
//! macOS 10.15 and later look for an embedded controller named `EC` and for USB power
//! properties on a `USBX` device; q35 has neither. The table adds both, for macOS alone
//! (`_OSI ("Darwin")`), as this ASL, which OpenCorePkg's SSDT-EC-USBX sample is, with `EC` under
//! q35's `\_SB.PCI0` in place of a laptop's `LPCB`:
//!
//! ```text
//! DefinitionBlock ("", "SSDT", 2, "REIMS ", "EcUsbx", 0x00001000)
//! {
//!     Scope (\_SB)
//!     {
//!         Device (USBX)
//!         {
//!             Name (_ADR, Zero)
//!             Method (_DSM, 4, NotSerialized)
//!             {
//!                 If ((Arg2 == Zero)) { Return (Buffer (One) { 0x03 }) }
//!                 Return (Package (0x08) {
//!                     "kUSBSleepPowerSupply", 0x13EC, "kUSBSleepPortCurrentLimit", 0x0834,
//!                     "kUSBWakePowerSupply", 0x13EC, "kUSBWakePortCurrentLimit", 0x0834 })
//!             }
//!             Method (_STA, 0, NotSerialized)
//!             {
//!                 If (_OSI ("Darwin")) { Return (0x0F) } Else { Return (Zero) }
//!             }
//!         }
//!     }
//!     Scope (\_SB.PCI0)
//!     {
//!         Device (EC)
//!         {
//!             Name (_HID, "ACID0001")
//!             Method (_STA, 0, NotSerialized)
//!             {
//!                 If (_OSI ("Darwin")) { Return (0x0F) } Else { Return (Zero) }
//!             }
//!         }
//!     }
//! }
//! ```

pub const FILE_NAME: &str = "SSDT-EC-USBX.aml";

const SCOPE_OP: u8 = 0x10;
const NAME_OP: u8 = 0x08;
const METHOD_OP: u8 = 0x14;
const DEVICE_OP: [u8; 2] = [0x5b, 0x82];
const IF_OP: u8 = 0xa0;
const ELSE_OP: u8 = 0xa1;
const RETURN_OP: u8 = 0xa4;
const LEQUAL_OP: u8 = 0x93;
const BUFFER_OP: u8 = 0x11;
const PACKAGE_OP: u8 = 0x12;
const ARG2_OP: u8 = 0x6a;

/// The table, header and checksum included.
pub fn ssdt_ec_usbx() -> Vec<u8> {
    let usbx = device(
        "USBX",
        [
            name("_ADR", integer(0)),
            method(
                "_DSM",
                4,
                [
                    if_(
                        [vec![LEQUAL_OP, ARG2_OP], integer(0)].concat(),
                        ret(pkg(&[BUFFER_OP], [integer(1), vec![0x03]].concat())),
                    ),
                    ret(pkg(
                        &[PACKAGE_OP],
                        [
                            vec![0x08],
                            string("kUSBSleepPowerSupply"),
                            integer(0x13ec),
                            string("kUSBSleepPortCurrentLimit"),
                            integer(0x0834),
                            string("kUSBWakePowerSupply"),
                            integer(0x13ec),
                            string("kUSBWakePortCurrentLimit"),
                            integer(0x0834),
                        ]
                        .concat(),
                    )),
                ]
                .concat(),
            ),
            sta_darwin_only(),
        ]
        .concat(),
    );
    let ec = device(
        "EC",
        [name("_HID", string("ACID0001")), sta_darwin_only()].concat(),
    );
    let body = [
        pkg(&[SCOPE_OP], [name_path("\\_SB"), usbx].concat()),
        pkg(&[SCOPE_OP], [name_path("\\_SB.PCI0"), ec].concat()),
    ]
    .concat();
    table(*b"SSDT", 2, *b"REIMS ", *b"EcUsbx  ", 0x1000, &body)
}

/// `Method (_STA) { If (_OSI ("Darwin")) { Return (0x0F) } Else { Return (Zero) } }`
fn sta_darwin_only() -> Vec<u8> {
    method(
        "_STA",
        0,
        [
            if_(
                [name_seg("_OSI").to_vec(), string("Darwin")].concat(),
                ret(integer(0x0f)),
            ),
            pkg(&[ELSE_OP], ret(integer(0))),
        ]
        .concat(),
    )
}

fn table(
    signature: [u8; 4],
    revision: u8,
    oem_id: [u8; 6],
    oem_table_id: [u8; 8],
    oem_revision: u32,
    body: &[u8],
) -> Vec<u8> {
    let mut t = Vec::with_capacity(36 + body.len());
    t.extend_from_slice(&signature);
    t.extend_from_slice(&((36 + body.len()) as u32).to_le_bytes());
    t.push(revision);
    t.push(0); // checksum, below
    t.extend_from_slice(&oem_id);
    t.extend_from_slice(&oem_table_id);
    t.extend_from_slice(&oem_revision.to_le_bytes());
    t.extend_from_slice(b"RUTL");
    t.extend_from_slice(&1u32.to_le_bytes());
    t.extend_from_slice(body);
    let sum = t.iter().fold(0u8, |a, b| a.wrapping_add(*b));
    t[9] = 0u8.wrapping_sub(sum);
    t
}

/// An op with a PkgLength: the length counts its own bytes and the body's.
fn pkg(op: &[u8], body: Vec<u8>) -> Vec<u8> {
    let n = body.len();
    let len_bytes = if n + 1 < 0x40 {
        1
    } else if n + 2 < 0x1000 {
        2
    } else if n + 3 < 0x10_0000 {
        3
    } else {
        4
    };
    let total = n + len_bytes;
    let mut v = op.to_vec();
    if len_bytes == 1 {
        v.push(total as u8);
    } else {
        v.push((((len_bytes - 1) as u8) << 6) | (total & 0x0f) as u8);
        for i in 0..len_bytes - 1 {
            v.push((total >> (4 + 8 * i)) as u8);
        }
    }
    v.extend(body);
    v
}

fn name_seg(s: &str) -> [u8; 4] {
    let mut seg = [b'_'; 4];
    for (d, c) in seg.iter_mut().zip(s.bytes()) {
        *d = c;
    }
    seg
}

/// A path from the root: `\_SB`, `\_SB.PCI0`.
fn name_path(path: &str) -> Vec<u8> {
    let segs: Vec<[u8; 4]> = path
        .trim_start_matches('\\')
        .split('.')
        .map(name_seg)
        .collect();
    let mut v = vec![b'\\'];
    match segs.len() {
        1 => {}
        2 => v.push(0x2e),
        n => {
            v.push(0x2f);
            v.push(n as u8);
        }
    }
    for s in segs {
        v.extend_from_slice(&s);
    }
    v
}

fn integer(x: u64) -> Vec<u8> {
    match x {
        0 => vec![0x00],
        1 => vec![0x01],
        2..=0xff => vec![0x0a, x as u8],
        0x100..=0xffff => [vec![0x0b], (x as u16).to_le_bytes().to_vec()].concat(),
        0x1_0000..=0xffff_ffff => [vec![0x0c], (x as u32).to_le_bytes().to_vec()].concat(),
        _ => [vec![0x0e], x.to_le_bytes().to_vec()].concat(),
    }
}

fn string(s: &str) -> Vec<u8> {
    [vec![0x0d], s.as_bytes().to_vec(), vec![0]].concat()
}

fn name(n: &str, value: Vec<u8>) -> Vec<u8> {
    [vec![NAME_OP], name_seg(n).to_vec(), value].concat()
}

fn device(n: &str, body: Vec<u8>) -> Vec<u8> {
    pkg(&DEVICE_OP, [name_seg(n).to_vec(), body].concat())
}

/// A NotSerialized method of `args` arguments.
fn method(n: &str, args: u8, body: Vec<u8>) -> Vec<u8> {
    pkg(
        &[METHOD_OP],
        [name_seg(n).to_vec(), vec![args & 7], body].concat(),
    )
}

fn if_(predicate: Vec<u8>, body: Vec<u8>) -> Vec<u8> {
    pkg(&[IF_OP], [predicate, body].concat())
}

fn ret(value: Vec<u8>) -> Vec<u8> {
    [vec![RETURN_OP], value].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unhex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn encodes_the_ec_device_as_iasl_does() {
        let ec = device(
            "EC",
            [name("_HID", string("ACID0001")), sta_darwin_only()].concat(),
        );
        assert_eq!(
            ec,
            unhex(
                "5b82 30 45435f5f 08 5f484944 0d 4143494430303031 00
                 14 1b 5f535441 00 a0 10 5f4f5349 0d 44617277696e 00 a4 0a0f a1 03 a4 00"
            )
        );
    }

    #[test]
    fn encodes_two_byte_package_lengths() {
        // 0x8f bytes in all: lead byte 0x4f (one more byte, low nibble f), then 0x08.
        let v = pkg(&DEVICE_OP, vec![0; 0x8d]);
        assert_eq!(&v[..4], &[0x5b, 0x82, 0x4f, 0x08]);
        assert_eq!(v.len(), 2 + 0x8f);
    }

    #[test]
    fn table_sums_to_zero() {
        let t = ssdt_ec_usbx();
        assert_eq!(&t[0..4], b"SSDT");
        assert_eq!(
            u32::from_le_bytes([t[4], t[5], t[6], t[7]]) as usize,
            t.len()
        );
        assert_eq!(t.iter().fold(0u8, |a, b| a.wrapping_add(*b)), 0);
        // The USBX method's return value starts as iasl writes it.
        let usbx = t.windows(4).position(|w| w == b"USBX").unwrap();
        assert_eq!(
            &t[usbx + 4..usbx + 10],
            &[0x08, b'_', b'A', b'D', b'R', 0x00]
        );
    }
}
