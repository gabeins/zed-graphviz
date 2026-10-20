//! Checking and unpacking the downloaded preview server.
//!
//! The server archive is downloaded as-is, checked against the SHA-256 in
//! the `.sha256` file published next to it on the same release, and only
//! then unpacked. A mismatch leaves nothing behind.

use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;

/// Written last into a finished installation; a folder without it is an
/// interrupted one and gets reinstalled.
pub const MARKER: &str = ".installed";
/// Shipped in every archive next to the binary.
pub const NOTICES: &str = "THIRD_PARTY_NOTICES.md";

/// Whether `dir` holds a finished installation with a non-empty `binary`.
pub fn is_complete(dir: &Path, binary: &str) -> bool {
    dir.join(MARKER).is_file()
        && fs::metadata(dir.join(binary)).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// Checks an unpacked `staging` folder (the binary is there and not empty,
/// the notices are there), marks it finished and renames it to `dir`,
/// replacing whatever unfinished installation was there.
pub fn commit_install(staging: &Path, dir: &Path, binary: &str) -> Result<(), String> {
    if !fs::metadata(staging.join(binary)).is_ok_and(|m| m.is_file() && m.len() > 0) {
        return Err(format!("the archive has no usable {binary}"));
    }
    if !staging.join(NOTICES).is_file() {
        return Err(format!("the archive has no {NOTICES}"));
    }
    fs::write(staging.join(MARKER), b"").map_err(|e| e.to_string())?;
    if dir.exists() {
        fs::remove_dir_all(dir).map_err(|e| format!("could not remove {}: {e}", dir.display()))?;
    }
    fs::rename(staging, dir).map_err(|e| format!("could not move the installation into place: {e}"))
}

/// Reads the checksum for `file_name` from a `sha256sum`-style file
/// (`<64 hex digits>  <file name>`).
pub fn parse_checksum(contents: &str, file_name: &str) -> Result<String, String> {
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let (Some(hash), name) = (fields.next(), fields.next()) else {
            continue;
        };
        let name = name.map(|n| n.trim_start_matches('*'));
        if name.is_some_and(|n| n != file_name) {
            continue;
        }
        if hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()) {
            return Ok(hash.to_ascii_lowercase());
        }
    }
    Err(format!("no SHA-256 for {file_name} in its checksum file"))
}

/// Checks that `bytes` hash to `expected` (hex).
pub fn verify_sha256(bytes: &[u8], expected: &str) -> Result<(), String> {
    let actual: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "checksum mismatch: expected {expected}, got {actual}"
        ))
    }
}

/// Unpacks a `.tar.gz` into `dir`. Entries that would land outside `dir`
/// are refused by `tar`.
pub fn unpack_tar_gz(bytes: &[u8], dir: &Path) -> Result<(), String> {
    let mut archive = tar::Archive::new(GzDecoder::new(bytes));
    let entries = archive.entries().map_err(|e| e.to_string())?;
    for entry in entries {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let unpacked = entry.unpack_in(dir).map_err(|e| e.to_string())?;
        if !unpacked {
            let path = entry
                .path()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            return Err(format!("the archive has an unsafe path: {path}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ABC_SHA256: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn parses_sha256sum_output() {
        let file = format!("{ABC_SHA256}  graphviz-preview-x86_64-unknown-linux-gnu.tar.gz\n");
        assert_eq!(
            parse_checksum(&file, "graphviz-preview-x86_64-unknown-linux-gnu.tar.gz").unwrap(),
            ABC_SHA256
        );
        // Binary-mode marker and upper case.
        let file = format!("{}  *a.tar.gz\n", ABC_SHA256.to_uppercase());
        assert_eq!(parse_checksum(&file, "a.tar.gz").unwrap(), ABC_SHA256);
        // A bare hash.
        assert_eq!(parse_checksum(ABC_SHA256, "a.tar.gz").unwrap(), ABC_SHA256);
    }

    #[test]
    fn rejects_other_files_and_malformed_hashes() {
        let file = format!("{ABC_SHA256}  other.tar.gz\n");
        assert!(parse_checksum(&file, "a.tar.gz").is_err());
        assert!(parse_checksum("abc123  a.tar.gz", "a.tar.gz").is_err());
        assert!(parse_checksum("", "a.tar.gz").is_err());
    }

    #[test]
    fn verifies_the_digest() {
        assert!(verify_sha256(b"abc", ABC_SHA256).is_ok());
        assert!(verify_sha256(b"abd", ABC_SHA256).is_err());
        assert!(verify_sha256(b"abc", &ABC_SHA256.to_uppercase()).is_ok());
    }

    fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::Regular);
            // Write the name directly so unsafe paths can be tested too.
            header.as_gnu_mut().unwrap().name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            builder.append(&header, *data).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut gz, &tar).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn unpacks_into_the_directory() {
        let dir = std::env::temp_dir().join(format!("zed-graphviz-unpack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let archive = tar_gz(&[("graphviz-preview", b"binary"), ("licenses/A.txt", b"text")]);
        unpack_tar_gz(&archive, &dir).unwrap();
        assert_eq!(
            std::fs::read(dir.join("graphviz-preview")).unwrap(),
            b"binary"
        );
        assert_eq!(std::fs::read(dir.join("licenses/A.txt")).unwrap(), b"text");

        let evil = tar_gz(&[("../escaped", b"x")]);
        assert!(unpack_tar_gz(&evil, &dir).is_err());
        assert!(!dir.parent().unwrap().join("escaped").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("zed-graphviz-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_half_extracted_installation_is_not_complete() {
        let root = scratch("half");
        let dir = root.join("graphviz-preview-v1");
        std::fs::create_dir_all(&dir).unwrap();
        // Extraction stopped after the binary: no marker.
        std::fs::write(dir.join("graphviz-preview"), b"binary").unwrap();
        assert!(!is_complete(&dir, "graphviz-preview"));
        // A marker but a truncated binary.
        std::fs::write(dir.join(MARKER), b"").unwrap();
        std::fs::write(dir.join("graphviz-preview"), b"").unwrap();
        assert!(!is_complete(&dir, "graphviz-preview"));

        // A fresh install replaces it.
        let staging = root.join("graphviz-preview-v1.staging-1");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("graphviz-preview"), b"binary").unwrap();
        std::fs::write(staging.join(NOTICES), b"notices").unwrap();
        commit_install(&staging, &dir, "graphviz-preview").unwrap();
        assert!(is_complete(&dir, "graphviz-preview"));
        assert!(!staging.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_incomplete_archive_is_not_installed() {
        let root = scratch("incomplete");
        let dir = root.join("graphviz-preview-v1");
        let staging = root.join("staging");
        std::fs::create_dir_all(&staging).unwrap();
        std::fs::write(staging.join("graphviz-preview"), b"binary").unwrap();
        assert!(
            commit_install(&staging, &dir, "graphviz-preview").is_err(),
            "no notices"
        );
        std::fs::write(staging.join(NOTICES), b"notices").unwrap();
        std::fs::write(staging.join("graphviz-preview"), b"").unwrap();
        assert!(
            commit_install(&staging, &dir, "graphviz-preview").is_err(),
            "empty binary"
        );
        assert!(!dir.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }
}
