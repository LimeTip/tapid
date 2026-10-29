use crate::{ArchiveFormat, ExtractError, ValidationLimits, validate_archive};
use std::io::{self, Cursor, Read};

/// Inspect a release without extracting or executing any member.
///
/// Release archives contain exactly `tapid` or `tapid.exe`, as selected by the
/// caller. Parser extensions, directories, links and trailing members are not
/// part of this deliberately narrower contract. Compressed and expanded input
/// are each bounded to 512 MiB, including tar framing when expanded.
pub fn inspect_release_archive(bytes: &[u8], executable: &str) -> Result<(), ExtractError> {
    const LIMIT: usize = 512 * 1024 * 1024;
    const FRAMING: u64 = 64 * 1024;
    let invalid =
        || ExtractError::InvalidArchive("expected exactly one regular release executable".into());
    if !matches!(executable, "tapid" | "tapid.exe") {
        return Err(invalid());
    }
    if bytes.len() > LIMIT {
        return Err(ExtractError::ArchiveTooLarge);
    }
    validate_archive(
        bytes,
        ArchiveFormat::TarGz,
        ValidationLimits {
            max_entries: 1,
            max_entry_size: LIMIT as u64,
            max_total_size: LIMIT as u64,
            max_path_bytes: 32,
        },
    )?;
    // The ordinary validator owns hostile header rules. This second, raw pass
    // enforces the release member contract and consumes the gzip trailer too.
    let decoder = flate2::read::MultiGzDecoder::new(Cursor::new(bytes));
    let expanded_limit = LIMIT as u64 + 1;
    let mut archive = tar::Archive::new(decoder.take(expanded_limit));
    let mut count = 0;
    let mut minimum_tar_bytes = 0;
    for entry in archive.entries()?.raw(true) {
        let mut entry = entry?;
        count += 1;
        if count != 1
            || entry.path_bytes().as_ref() != executable.as_bytes()
            || !entry.header().entry_type().is_file()
            || entry.size() == 0
            || executable == "tapid" && entry.header().mode()? & 0o111 == 0
        {
            return Err(invalid());
        }
        let size = entry.size();
        minimum_tar_bytes = 512 + size.div_ceil(512) * 512 + 1024;
        if io::copy(&mut entry, &mut io::sink())? != size {
            return Err(invalid());
        }
    }
    if count != 1 {
        return Err(invalid());
    }
    let mut reader = archive.into_inner();
    let consumed = expanded_limit - reader.limit();
    let mut trailing = 0u64;
    let mut buffer = [0; 8192];
    loop {
        let size = reader.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        trailing += size as u64;
        if trailing > FRAMING || buffer[..size].iter().any(|byte| *byte != 0) {
            return Err(invalid());
        }
    }
    if consumed + trailing < minimum_tar_bytes || consumed + trailing > LIMIT as u64 {
        return Err(invalid());
    }
    Ok(())
}

#[cfg(test)]
mod release_inspection_tests {
    use super::*;
    use std::io::Write;

    fn archive(names: &[&str]) -> Vec<u8> {
        let encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(encoder);
        for name in names {
            let mut header = tar::Header::new_gnu();
            header.set_size(6);
            header.set_mode(0o755);
            header.set_cksum();
            builder
                .append_data(&mut header, name, &b"binary"[..])
                .unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    #[test]
    fn release_inspection_requires_one_exact_regular_executable() {
        assert!(inspect_release_archive(&archive(&["tapid"]), "tapid").is_ok());
        assert!(inspect_release_archive(&archive(&["tapid.exe"]), "tapid.exe").is_ok());
        for names in [
            vec![],
            vec!["tapid.exe"],
            vec!["tapid", "extra"],
            vec!["dir/tapid"],
        ] {
            assert!(inspect_release_archive(&archive(&names), "tapid").is_err());
        }
        let mut truncated = archive(&["tapid"]);
        truncated.truncate(truncated.len() - 8);
        assert!(inspect_release_archive(&truncated, "tapid").is_err());
        let mut extra = archive(&["tapid"]);
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(b"hidden trailing content").unwrap();
        extra.extend(encoder.finish().unwrap());
        assert!(inspect_release_archive(&extra, "tapid").is_err());
    }
}

#[cfg(test)]
mod framing_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn release_inspection_rejects_a_gzip_with_missing_tar_terminators() {
        let mut header = tar::Header::new_gnu();
        header.set_path("tapid").unwrap();
        header.set_size(1);
        header.set_mode(0o755);
        header.set_cksum();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        encoder.write_all(header.as_bytes()).unwrap();
        let mut payload = [0; 512];
        payload[0] = b'x';
        encoder.write_all(&payload).unwrap();
        assert!(inspect_release_archive(&encoder.finish().unwrap(), "tapid").is_err());
    }
}

#[cfg(test)]
mod hostile_member_tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn release_inspection_rejects_links_special_files_modes_and_declared_expansion() {
        for (kind, mode, size) in [
            (b'2', 0o755, 0),
            (b'1', 0o755, 0),
            (b'5', 0o755, 0),
            (b'3', 0o755, 0),
            (b'0', 0o644, 1),
            (b'0', 0o755, 512 * 1024 * 1024 + 1),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_path("tapid").unwrap();
            header.set_entry_type(tar::EntryType::new(kind));
            header.set_link_name("target").unwrap();
            header.set_mode(mode);
            header.set_size(size);
            header.set_cksum();
            let mut encoder =
                flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
            encoder.write_all(header.as_bytes()).unwrap();
            encoder.write_all(&[0; 2048]).unwrap();
            assert!(inspect_release_archive(&encoder.finish().unwrap(), "tapid").is_err());
        }
    }
}
