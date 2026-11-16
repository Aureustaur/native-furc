//! Parser and extractor for Furcadia `FR01` (`.rch`) compressed dream packages.

use std::fmt;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub enum RchError {
    InvalidHeader,
    TruncatedEntry,
    InvalidFileName,
    DecompressFailed(String),
    Io(std::io::Error),
}

impl fmt::Display for RchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidHeader => write!(f, "invalid FR01/RCH archive header"),
            Self::TruncatedEntry => write!(f, "truncated FR01/RCH entry"),
            Self::InvalidFileName => write!(f, "unsafe or invalid filename in FR01 archive"),
            Self::DecompressFailed(msg) => write!(f, "FR01 decompression failed: {msg}"),
            Self::Io(err) => write!(f, "I/O error: {err}"),
        }
    }
}

impl std::error::Error for RchError {}

impl From<std::io::Error> for RchError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

#[derive(Debug, Clone)]
pub struct RchEntry {
    pub raw_name: String,
    pub clean_name: String,
    pub compressed_type: u32,
    pub data: Vec<u8>,
}

/// Returns true if `bytes` begins with the `FR01` Furcadia dream package signature.
pub fn is_fr01(bytes: &[u8]) -> bool {
    bytes.len() >= 28 && &bytes[0..4] == b"FR01"
}

/// Parses and decompresses all entries inside a Furcadia `FR01` (`.rch`) package.
pub fn parse_fr01(bytes: &[u8]) -> Result<Vec<RchEntry>, RchError> {
    if !is_fr01(bytes) {
        return Err(RchError::InvalidHeader);
    }

    let mut pos = 28usize;
    let mut entries = Vec::new();

    while pos + 62 <= bytes.len() {
        let name_field = &bytes[pos..pos + 42];
        let name_end = name_field.iter().position(|&b| b == 0).unwrap_or(42);
        let raw_name = String::from_utf8_lossy(&name_field[..name_end]).into_owned();
        if raw_name.is_empty() {
            return Err(RchError::InvalidFileName);
        }

        let meta = &bytes[pos + 42..pos + 62];
        let comp_size = u32::from_le_bytes([meta[8], meta[9], meta[10], meta[11]]) as usize;
        let comp_type = u32::from_le_bytes([meta[16], meta[17], meta[18], meta[19]]);
        pos += 62;

        if pos + comp_size > bytes.len() {
            return Err(RchError::TruncatedEntry);
        }
        let payload = &bytes[pos..pos + comp_size];
        pos += comp_size;

        let data = match comp_type {
            0 => payload.to_vec(),
            1 => payload.iter().map(|&b| b ^ 0xff).collect(),
            2 => decompress_bzip2(payload)?,
            other => {
                return Err(RchError::DecompressFailed(format!(
                    "unsupported FR01 compression type {other} for '{raw_name}'"
                )));
            }
        };

        let stripped = raw_name.strip_prefix("FZ").unwrap_or(&raw_name);
        let clean_name = sanitize_rel_filename(stripped)?;

        entries.push(RchEntry {
            raw_name,
            clean_name,
            compressed_type: comp_type,
            data,
        });
    }

    Ok(entries)
}

/// Extracts an `FR01` archive into `dest_dir`, returning paths to all written files.
///
/// Strips the `FZ` transport prefix from extracted filenames (e.g. `FZdefault.map` -> `default.map`).
pub fn extract_fr01_to_dir(bytes: &[u8], dest_dir: &Path) -> Result<Vec<PathBuf>, RchError> {
    let entries = parse_fr01(bytes)?;
    fs::create_dir_all(dest_dir)?;

    let mut written = Vec::with_capacity(entries.len());
    for entry in entries {
        let out_path = dest_dir.join(&entry.clean_name);
        fs::write(&out_path, &entry.data)?;
        written.push(out_path);
    }
    Ok(written)
}

fn sanitize_rel_filename(name: &str) -> Result<String, RchError> {
    let base = Path::new(name)
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or(RchError::InvalidFileName)?;
    if base.is_empty() || base == "." || base == ".." || base.contains('/') || base.contains('\\') {
        return Err(RchError::InvalidFileName);
    }
    Ok(base.to_owned())
}

fn decompress_bzip2(payload: &[u8]) -> Result<Vec<u8>, RchError> {
    let mut decoder = bzip2_rs::DecoderReader::new(payload);
    let mut out = Vec::new();
    decoder
        .read_to_end(&mut out)
        .map_err(|e| RchError::DecompressFailed(format!("bzip2 decode failed: {e}")))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_synthetic_fr01_xor_entry() {
        let mut archive = Vec::new();
        archive.extend_from_slice(b"FR01");
        archive.extend_from_slice(&[0u8; 24]);

        let mut name = [0u8; 42];
        let raw_name = b"FZtest.txt";
        name[..raw_name.len()].copy_from_slice(raw_name);
        archive.extend_from_slice(&name);

        let plain = b"hello furcadia";
        let xor_payload: Vec<u8> = plain.iter().map(|&b| b ^ 0xff).collect();
        archive.extend_from_slice(&0u32.to_le_bytes()); // f1
        archive.extend_from_slice(&0u32.to_le_bytes()); // f2
        archive.extend_from_slice(&(xor_payload.len() as u32).to_le_bytes()); // comp_size
        archive.extend_from_slice(&0u32.to_le_bytes()); // crc
        archive.extend_from_slice(&1u32.to_le_bytes()); // comp_type = 1 (XOR 0xff)
        archive.extend_from_slice(&xor_payload);

        let entries = parse_fr01(&archive).expect("synthetic FR01 should parse");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].clean_name, "test.txt");
        assert_eq!(entries[0].data, plain);
    }

    #[test]
    fn parses_live_pmfurn_fr01_if_present() {
        let path = Path::new("/tmp/pmfurn.bin");
        if !path.exists() {
            return;
        }
        let bytes = fs::read(path).expect("read /tmp/pmfurn.bin");
        let entries = parse_fr01(&bytes).expect("parse pmfurn FR01");
        let names: Vec<&str> = entries.iter().map(|e| e.clean_name.as_str()).collect();
        assert!(names.contains(&"default.map"), "missing default.map in {names:?}");
        assert!(names.contains(&"floore.fox"), "missing floore.fox in {names:?}");
        assert!(names.contains(&"iteme.fox"), "missing iteme.fox in {names:?}");
    }
}
