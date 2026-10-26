//! Strict release archives have one original regular binary member.
use super::*;

/// Inspect a release tar.gz without extracting or executing its foreign binary.
/// Exactly one original regular member is permitted; extension headers are rejected.
pub fn validate_executable_archive(
    bytes: &[u8],
    expected_name: &str,
    limits: ArchiveLimits,
) -> Result<(), ExtractError> {
    if !matches!(expected_name, "tapid" | "tapid.exe") {
        return Err(ExtractError::InvalidArchive(
            "invalid executable name".into(),
        ));
    }
    if bytes.len() > limits.max_archive_bytes {
        return Err(ExtractError::ArchiveTooLarge);
    }
    validate_initial_metadata(bytes, ArchiveFormat::TarGz, limits.validation)?;
    validate_archive(bytes, ArchiveFormat::TarGz, limits.validation)?;
    let remaining = Cell::new(PARSER_METADATA_LIMIT);
    let mut archive = tar::Archive::new(ParserBudgetReader {
        reader: flate2::read::MultiGzDecoder::new(Cursor::new(bytes)),
        remaining: &remaining,
    });
    // Do not let a zero block conceal another tar member or gzip stream.
    archive.set_ignore_zeros(true);
    let mut count = 0;
    for item in archive.entries()?.raw(true) {
        let mut entry = item?;
        count += 1;
        if count != 1
            || !entry.header().entry_type().is_file()
            || entry.path_bytes().as_ref() != expected_name.as_bytes()
            || entry.size() == 0
            || entry.size() > limits.validation.max_entry_size
            || entry.size() > limits.validation.max_total_size
        {
            return Err(ExtractError::InvalidArchive(
                "expected exactly one original regular executable member".into(),
            ));
        }
        remaining.set(entry.size().saturating_add(PARSER_METADATA_LIMIT));
        io::copy(&mut entry, &mut io::sink())?;
        remaining.set(PARSER_METADATA_LIMIT);
    }
    if count != 1 {
        return Err(ExtractError::InvalidArchive(
            "missing executable member".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(names: &[(&str, u8)]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for &(name, kind) in names {
            let mut header = tar::Header::new_ustar();
            header.set_size(if kind == b'0' { 1 } else { 0 });
            header.set_mode(0o755);
            header.set_entry_type(tar::EntryType::new(kind));
            header.as_mut_bytes()[..100].fill(0);
            header.as_mut_bytes()[..name.len()].copy_from_slice(name.as_bytes());
            header.set_cksum();
            builder
                .append(&header, if kind == b'0' { &b"x"[..] } else { &[] })
                .unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut gzip, &tar).unwrap();
        gzip.finish().unwrap()
    }

    #[test]
    fn executable_archive_requires_one_exact_original_regular_member() {
        for name in ["tapid", "tapid.exe"] {
            assert!(
                validate_executable_archive(
                    &archive(&[(name, b'0')]),
                    name,
                    ArchiveLimits::default()
                )
                .is_ok()
            );
            for entries in [
                vec![],
                vec![(name, b'0'), (name, b'0')],
                vec![(name, b'0'), ("extra", b'0')],
                vec![(name, b'0'), ("extra", b'5')],
                vec![(name, b'2')],
                vec![(".tapid-executable-modes", b'0')],
                vec![(
                    if name == "tapid" {
                        "tapid.exe"
                    } else {
                        "tapid"
                    },
                    b'0',
                )],
                vec![("./tapid", b'0')],
                vec![("../tapid", b'0')],
                vec![("/tapid", b'0')],
                vec![("C:\\tapid", b'0')],
                vec![(name, b'1')],
                vec![(name, b'3')],
                vec![(name, b'6')],
                vec![(name, b'x')],
            ] {
                assert!(
                    validate_executable_archive(&archive(&entries), name, ArchiveLimits::default())
                        .is_err(),
                    "{entries:?}"
                );
            }
        }
    }

    #[test]
    fn executable_archive_bounds_payload_and_rejects_hidden_streams_and_truncation() {
        let bytes = archive(&[("tapid", b'0')]);
        let compressed_limit = ArchiveLimits {
            max_archive_bytes: bytes.len() - 1,
            ..ArchiveLimits::default()
        };
        assert!(validate_executable_archive(&bytes, "tapid", compressed_limit).is_err());
        let mut extracted_limit = ArchiveLimits::default();
        extracted_limit.validation.max_entry_size = 0;
        assert!(validate_executable_archive(&bytes, "tapid", extracted_limit).is_err());
        let mut appended = bytes.clone();
        appended.extend_from_slice(&archive(&[("extra", b'0')]));
        assert!(validate_executable_archive(&appended, "tapid", ArchiveLimits::default()).is_err());
        for length in [0, 10, bytes.len() - 1] {
            assert!(
                validate_executable_archive(&bytes[..length], "tapid", ArchiveLimits::default())
                    .is_err()
            );
        }
    }
}
