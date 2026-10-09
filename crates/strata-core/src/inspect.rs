//! File inspection for the metadata pane: executable architecture and
//! MD5 checksums.

use std::io::Read;
use std::path::Path;

use anyhow::Result;
use md5::{Digest, Md5};

use crate::jobs::Progress;
use crate::Vfs;

/// Describes an executable from its header: `ELF 64-bit x86-64`,
/// `PE32+ ARM64`, `Mach-O 64-bit arm64`... `None` for other files.
pub fn binary_arch(header: &[u8]) -> Option<String> {
    let u16_at = |i: usize, le: bool| -> Option<u16> {
        let b: [u8; 2] = header.get(i..i + 2)?.try_into().ok()?;
        Some(if le { u16::from_le_bytes(b) } else { u16::from_be_bytes(b) })
    };
    let u32_at = |i: usize, le: bool| -> Option<u32> {
        let b: [u8; 4] = header.get(i..i + 4)?.try_into().ok()?;
        Some(if le { u32::from_le_bytes(b) } else { u32::from_be_bytes(b) })
    };

    if header.starts_with(b"\x7fELF") {
        let bits = match header.get(4)? {
            1 => "32-bit",
            2 => "64-bit",
            _ => "",
        };
        let le = *header.get(5)? == 1;
        let machine = match u16_at(18, le)? {
            0x03 => "x86",
            0x3E => "x86-64",
            0x28 => "ARM",
            0xB7 => "AArch64",
            0xF3 => "RISC-V",
            0x08 => "MIPS",
            0x14 => "PowerPC",
            0x15 => "PowerPC64",
            0x16 => "s390",
            0x102 => "LoongArch",
            other => return Some(format!("ELF {bits} machine {other:#x}")),
        };
        let kind = match u16_at(16, le)? {
            1 => " object",
            2 => " executable",
            3 => " shared object",
            4 => " core dump",
            _ => "",
        };
        return Some(format!("ELF {bits} {machine}{kind}"));
    }

    if header.starts_with(b"MZ") {
        let pe = u32_at(0x3C, true)? as usize;
        if header.get(pe..pe + 4)? != b"PE\0\0" {
            return Some("DOS executable".into());
        }
        let machine = match u16_at(pe + 4, true)? {
            0x14c => "x86",
            0x8664 => "x86-64",
            0x1c0 | 0x1c4 => "ARM",
            0xaa64 => "ARM64",
            other => return Some(format!("PE machine {other:#x}")),
        };
        let format = match u16_at(pe + 24, true) {
            Some(0x20b) => "PE32+",
            _ => "PE32",
        };
        return Some(format!("{format} {machine}"));
    }

    let magic = u32_at(0, false)?;
    let (bits, le) = match magic {
        0xFEED_FACE => ("32-bit", false),
        0xFEED_FACF => ("64-bit", false),
        0xCEFA_EDFE => ("32-bit", true),
        0xCFFA_EDFE => ("64-bit", true),
        0xCAFE_BABE => {
            // Shared with Java class files, which store a version (>= 45) here.
            let count = u32_at(4, false)?;
            return (count < 30).then(|| format!("Mach-O universal ({count} architectures)"));
        }
        _ => return None,
    };
    let cpu = match u32_at(4, le)? {
        7 => "x86",
        0x0100_0007 => "x86-64",
        12 => "arm",
        0x0100_000C => "arm64",
        18 => "ppc",
        other => return Some(format!("Mach-O {bits} cpu {other:#x}")),
    };
    Some(format!("Mach-O {bits} {cpu}"))
}

/// Reads enough of a file to identify executables.
pub fn file_arch(vfs: &dyn Vfs, path: &Path) -> Option<String> {
    let mut header = Vec::with_capacity(4096);
    vfs.reader(path).ok()?.take(4096).read_to_end(&mut header).ok()?;
    binary_arch(&header)
}

/// Lower-case hex MD5 of a file. Honours cancellation via `progress`.
pub fn md5(vfs: &dyn Vfs, path: &Path, progress: &Progress) -> Result<String> {
    let mut reader = vfs.reader(path)?;
    let mut hasher = Md5::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        progress.check()?;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        progress.add_bytes(n as u64);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vfs::LocalVfs;

    #[test]
    fn detects_elf() {
        let mut h = vec![0u8; 64];
        h[..4].copy_from_slice(b"\x7fELF");
        h[4] = 2;
        h[5] = 1;
        h[16] = 3;
        h[18] = 0x3E;
        assert_eq!(binary_arch(&h).unwrap(), "ELF 64-bit x86-64 shared object");
    }

    #[test]
    fn detects_pe() {
        let mut h = vec![0u8; 256];
        h[..2].copy_from_slice(b"MZ");
        h[0x3C] = 0x80;
        h[0x80..0x84].copy_from_slice(b"PE\0\0");
        h[0x84..0x86].copy_from_slice(&0xaa64u16.to_le_bytes());
        h[0x98..0x9A].copy_from_slice(&0x20bu16.to_le_bytes());
        assert_eq!(binary_arch(&h).unwrap(), "PE32+ ARM64");
    }

    #[test]
    fn detects_mach_o_and_ignores_java() {
        let mut h = vec![0u8; 16];
        h[..4].copy_from_slice(&0xCFFA_EDFEu32.to_be_bytes());
        h[4..8].copy_from_slice(&0x0100_000Cu32.to_le_bytes());
        assert_eq!(binary_arch(&h).unwrap(), "Mach-O 64-bit arm64");
        let java = [0xCA, 0xFE, 0xBA, 0xBE, 0, 0, 0, 61];
        assert_eq!(binary_arch(&java), None);
        assert_eq!(binary_arch(b"plain text"), None);
    }

    #[test]
    fn this_test_binary_is_recognised() {
        let exe = std::env::current_exe().unwrap();
        assert!(file_arch(&LocalVfs, &exe).is_some());
    }

    #[test]
    fn md5_matches_known_value() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a");
        std::fs::write(&f, "hello world").unwrap();
        assert_eq!(md5(&LocalVfs, &f, &Progress::default()).unwrap(), "5eb63bbbe01eeed093cb22bb8f5acdc3");
    }
}
