//! File inspection: executable architecture, checksums and checksum-file
//! verification.

use std::io::Read;
use std::path::Path;

use anyhow::Result;
use md5::{Digest, Md5};
use sha1::Sha1;
use sha2::{Sha256, Sha512};

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

/// A checksum algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HashAlgo {
    Md5,
    Sha1,
    Sha256,
    Sha512,
}

impl HashAlgo {
    pub const ALL: [HashAlgo; 4] = [HashAlgo::Md5, HashAlgo::Sha1, HashAlgo::Sha256, HashAlgo::Sha512];

    pub fn name(self) -> &'static str {
        match self {
            HashAlgo::Md5 => "MD5",
            HashAlgo::Sha1 => "SHA-1",
            HashAlgo::Sha256 => "SHA-256",
            HashAlgo::Sha512 => "SHA-512",
        }
    }

    /// The algorithm whose hex digest has `len` characters.
    pub fn from_hex_len(len: usize) -> Option<Self> {
        Self::ALL.into_iter().find(|a| a.hex_len() == len)
    }

    pub fn hex_len(self) -> usize {
        match self {
            HashAlgo::Md5 => 32,
            HashAlgo::Sha1 => 40,
            HashAlgo::Sha256 => 64,
            HashAlgo::Sha512 => 128,
        }
    }
}

enum Hasher {
    Md5(Md5),
    Sha1(Sha1),
    Sha256(Sha256),
    Sha512(Sha512),
}

impl Hasher {
    fn new(algo: HashAlgo) -> Self {
        match algo {
            HashAlgo::Md5 => Hasher::Md5(Md5::new()),
            HashAlgo::Sha1 => Hasher::Sha1(Sha1::new()),
            HashAlgo::Sha256 => Hasher::Sha256(Sha256::new()),
            HashAlgo::Sha512 => Hasher::Sha512(Sha512::new()),
        }
    }

    fn update(&mut self, data: &[u8]) {
        match self {
            Hasher::Md5(h) => h.update(data),
            Hasher::Sha1(h) => h.update(data),
            Hasher::Sha256(h) => h.update(data),
            Hasher::Sha512(h) => h.update(data),
        }
    }

    fn hex(self) -> String {
        let bytes: Vec<u8> = match self {
            Hasher::Md5(h) => h.finalize().to_vec(),
            Hasher::Sha1(h) => h.finalize().to_vec(),
            Hasher::Sha256(h) => h.finalize().to_vec(),
            Hasher::Sha512(h) => h.finalize().to_vec(),
        };
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
}

/// Lower-case hex digests of a file for each of `algos`, read in one pass.
/// Honours cancellation via `progress`.
pub fn hash_file(vfs: &dyn Vfs, path: &Path, algos: &[HashAlgo], progress: &Progress) -> Result<Vec<String>> {
    let mut reader = vfs.reader(path)?;
    let mut hashers: Vec<Hasher> = algos.iter().map(|a| Hasher::new(*a)).collect();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        progress.check()?;
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hashers.iter_mut().for_each(|h| h.update(&buf[..n]));
        progress.add_bytes(n as u64);
    }
    Ok(hashers.into_iter().map(Hasher::hex).collect())
}

/// Lower-case hex MD5 of a file. Honours cancellation via `progress`.
pub fn md5(vfs: &dyn Vfs, path: &Path, progress: &Progress) -> Result<String> {
    Ok(hash_file(vfs, path, &[HashAlgo::Md5], progress)?.remove(0))
}

/// True for names like `SHA256SUMS`, `app.tar.gz.sha256` or `files.md5`.
pub fn is_checksum_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, e)| e).unwrap_or("");
    matches!(ext, "md5" | "sha1" | "sha256" | "sha512" | "md5sum" | "sha1sum" | "sha256sum" | "sha512sum")
        || ["md5sums", "sha1sums", "sha256sums", "sha512sums", "checksums"].iter().any(|n| lower.starts_with(n))
}

/// One line of a checksum file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SumLine {
    pub algo: HashAlgo,
    /// Lower-case hex digest.
    pub hash: String,
    /// File name, relative to the checksum file's directory.
    pub file: String,
}

/// Parses GNU (`<hash>  file`, `<hash> *file`) and BSD
/// (`SHA256 (file) = <hash>`) checksum lines. A bare hash (as in
/// `app.tar.gz.sha256` files that hold only the digest) applies to
/// `default_file`.
pub fn parse_sums(text: &str, default_file: Option<&str>) -> Vec<SumLine> {
    let is_hex = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_hexdigit());
    let mut out = Vec::new();
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let parsed = if let Some((left, hash)) = line.rsplit_once(" = ").filter(|(l, _)| l.ends_with(')')) {
            left.split_once(" (").map(|(_, file)| (hash.trim(), file.trim_end_matches(')').to_string()))
        } else {
            match line.split_once(char::is_whitespace) {
                Some((hash, file)) => {
                    let file = file.trim_start();
                    Some((hash, file.strip_prefix('*').unwrap_or(file).to_string()))
                }
                None => default_file.map(|f| (line, f.to_string())),
            }
        };
        let Some((hash, file)) = parsed else { continue };
        if let Some(algo) = HashAlgo::from_hex_len(hash.len()).filter(|_| is_hex(hash) && !file.is_empty()) {
            out.push(SumLine { algo, hash: hash.to_ascii_lowercase(), file });
        }
    }
    out
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

    #[test]
    fn hashes_with_several_algorithms_at_once() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a");
        std::fs::write(&f, "hello world").unwrap();
        let sums = hash_file(&LocalVfs, &f, &[HashAlgo::Sha1, HashAlgo::Sha256], &Progress::default()).unwrap();
        assert_eq!(sums[0], "2aae6c35c94fcfb415dbe95f408b9ce91ee846ed");
        assert_eq!(sums[1], "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9");
    }

    #[test]
    fn parses_gnu_bsd_and_bare_checksum_lines() {
        let sha = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";
        let text = format!("{sha}  a.txt\n{sha} *b.bin\n# comment\nSHA256 (c d.txt) = {sha}\nnot a line\n");
        let lines = parse_sums(&text, None);
        let files: Vec<&str> = lines.iter().map(|l| l.file.as_str()).collect();
        assert_eq!(files, ["a.txt", "b.bin", "c d.txt"]);
        assert!(lines.iter().all(|l| l.algo == HashAlgo::Sha256));
        let bare = parse_sums("5EB63BBBE01EEED093CB22BB8F5ACDC3\n", Some("x.iso"));
        assert_eq!(
            bare,
            [SumLine { algo: HashAlgo::Md5, hash: "5eb63bbbe01eeed093cb22bb8f5acdc3".into(), file: "x.iso".into() }]
        );
    }

    #[test]
    fn recognises_checksum_files() {
        assert!(is_checksum_file("SHA256SUMS"));
        assert!(is_checksum_file("app.tar.gz.sha256"));
        assert!(is_checksum_file("files.md5"));
        assert!(!is_checksum_file("notes.txt"));
    }
}
