//! Consumption measurements exercise the same private traversal as extraction.
use super::*;
use std::io::Write;

struct GuardedReader<'a, R> {
    inner: R,
    consumed: &'a Cell<usize>,
    stop: usize,
}

impl<R: Read> Read for GuardedReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if buffer.is_empty() {
            return Ok(0);
        }
        let available = self.stop.saturating_sub(self.consumed.get());
        assert!(
            available > 0,
            "parser traversed guarded payload or later entry"
        );
        let length = available.min(buffer.len());
        let read = self.inner.read(&mut buffer[..length])?;
        self.consumed.set(self.consumed.get() + read);
        Ok(read)
    }
}

fn header(path: &str, size: u64, kind: u8) -> tar::Header {
    let mut header = tar::Header::new_gnu();
    header.set_path(path).unwrap();
    header.set_size(size);
    header.set_entry_type(tar::EntryType::new(kind));
    header.set_mode(0o644);
    header.set_cksum();
    header
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), Default::default());
    encoder.write_all(bytes).unwrap();
    encoder.finish().unwrap()
}

// The counter is AFTER decoding and BEFORE ParserBudgetReader. GzDecoder may
// buffer compressed source bytes; these assertions concern decompressed bytes
// actually delivered to the parser, not zero compressed-source read-ahead.
fn readers(bytes: &[u8]) -> Vec<Box<dyn Read + '_>> {
    vec![
        Box::new(Cursor::new(bytes)),
        Box::new(flate2::read::GzDecoder::new(Cursor::new(gzip(bytes)))),
    ]
}

#[test]
fn logical_limits_stop_at_the_violating_header_for_tar_and_gzip() {
    let defaults = ValidationLimits::default();
    for (limits, expected, prior, pax) in [
        (
            ValidationLimits {
                max_entries: 1,
                ..defaults
            },
            ValidationError::TooManyEntries,
            true,
            false,
        ),
        (
            ValidationLimits {
                max_path_bytes: 3,
                ..defaults
            },
            ValidationError::PathTooLong,
            false,
            false,
        ),
        (
            ValidationLimits {
                max_entry_size: 5,
                ..defaults
            },
            ValidationError::EntryTooLarge {
                path: "second".into(),
                size: 6,
            },
            false,
            false,
        ),
        (
            ValidationLimits {
                max_total_size: 10,
                ..defaults
            },
            ValidationError::TotalSizeExceeded,
            true,
            false,
        ),
        (
            ValidationLimits {
                max_entry_size: 5,
                ..defaults
            },
            ValidationError::EntryTooLarge {
                path: "second".into(),
                size: 6,
            },
            false,
            true,
        ),
        (
            ValidationLimits {
                max_total_size: 10,
                ..defaults
            },
            ValidationError::TotalSizeExceeded,
            true,
            true,
        ),
    ] {
        let mut bytes = Vec::new();
        if prior {
            bytes.extend_from_slice(header("first", 6, b'0').as_bytes());
            bytes.extend_from_slice(&[b'x'; 512]);
        }
        if pax {
            let mut builder = tar::Builder::new(Vec::new());
            builder
                .append_pax_extensions([("size", b"6".as_slice())])
                .unwrap();
            let extension = builder.into_inner().unwrap();
            bytes.extend_from_slice(&extension[..1024]);
        }
        bytes.extend_from_slice(header("second", if pax { 0 } else { 6 }, b'0').as_bytes());
        let stop = bytes.len();
        bytes.extend_from_slice(&[b'x'; 512]);
        bytes.extend_from_slice(header("later", 0, b'0').as_bytes());
        for reader in readers(&bytes) {
            let consumed = Cell::new(0);
            let error = validate_archive_reader(
                GuardedReader {
                    inner: reader,
                    consumed: &consumed,
                    stop,
                },
                limits,
            )
            .unwrap_err();
            assert!(
                matches!(error, ExtractError::Invalid(ref actual) if actual == &expected),
                "{error}"
            );
            assert_eq!(consumed.get(), stop);
        }
    }
}

#[test]
fn hidden_metadata_consumption_is_bounded_before_and_between_entries() {
    for kind in b"LKx" {
        for prior in [false, true] {
            // A single large declaration and repeated bounded records both
            // exercise metadata that tar consumes internally before yielding.
            for repeated in [false, true] {
                let mut bytes = Vec::new();
                if prior {
                    bytes.extend_from_slice(header("first", 1, b'0').as_bytes());
                    bytes.extend_from_slice(&[b'x'; 512]);
                }
                for _ in 0..if repeated { 80 } else { 1 } {
                    let size = if repeated { 1024 } else { 128 * 1024 };
                    bytes.extend_from_slice(header("metadata", size, *kind).as_bytes());
                    // Valid inert PAX records, or terminated GNU strings.
                    let body = if *kind == b'x' {
                        b"16 comment=test\n".repeat(size as usize / 16)
                    } else {
                        let mut body = vec![b'a'; size as usize];
                        *body.last_mut().unwrap() = 0;
                        body
                    };
                    bytes.extend_from_slice(&body);
                }
                bytes.extend_from_slice(header("later", 0, b'0').as_bytes());
                // The preceding one-byte payload gets one byte of budget; its
                // 511 padding bytes are charged to the metadata allowance.
                let stop = if prior { 66049 } else { 65536 };
                for reader in readers(&bytes) {
                    let consumed = Cell::new(0);
                    let error = validate_archive_reader(
                        GuardedReader {
                            inner: reader,
                            consumed: &consumed,
                            stop,
                        },
                        ValidationLimits::default(),
                    )
                    .unwrap_err();
                    if repeated {
                        // The pinned parser rejects duplicate extensions even
                        // earlier: one 1536-byte record plus the second header.
                        assert!(error.to_string().contains("two "), "{error}");
                        assert_eq!(consumed.get(), if prior { 1024 } else { 0 } + 2048);
                    } else {
                        assert!(
                            error.to_string().contains("parser metadata limit"),
                            "kind {kind}: {error}"
                        );
                        assert_eq!(consumed.get(), stop);
                    }
                }
            }
        }
    }
}

#[test]
fn leading_extension_declarations_stop_before_their_bodies() {
    for (kind, size) in [(b'L', 4098), (b'K', 4098), (b'x', 65537), (b'g', 1025)] {
        let mut bytes = header("metadata", size, kind).as_bytes().to_vec();
        bytes.extend_from_slice(&[b'x'; 1024]);
        for reader in readers(&bytes) {
            let consumed = Cell::new(0);
            let error = validate_initial_metadata_reader(
                GuardedReader {
                    inner: reader,
                    consumed: &consumed,
                    stop: 512,
                },
                ValidationLimits::default(),
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("parser metadata limit"),
                "{error}"
            );
            assert_eq!(consumed.get(), 512);
        }
    }
}

#[test]
fn mixed_bounded_extensions_share_one_parser_budget() {
    for prior in [false, true] {
        let mut bytes = Vec::new();
        if prior {
            bytes.extend_from_slice(header("first", 1, b'0').as_bytes());
            bytes.extend_from_slice(&[b'x'; 512]);
        }
        for kind in b"LKx" {
            bytes.extend_from_slice(header("metadata", 24 * 1024, *kind).as_bytes());
            if *kind == b'x' {
                bytes.extend_from_slice(&b"16 comment=test\n".repeat(1536));
            } else {
                bytes.extend_from_slice(&vec![b'a'; 24 * 1024 - 1]);
                bytes.push(0);
            }
        }
        bytes.extend_from_slice(header("later", 0, b'0').as_bytes());
        let stop = if prior { 66049 } else { 65536 };
        for reader in readers(&bytes) {
            let consumed = Cell::new(0);
            let error = validate_archive_reader(
                GuardedReader {
                    inner: reader,
                    consumed: &consumed,
                    stop,
                },
                ValidationLimits {
                    max_path_bytes: 32 * 1024,
                    ..Default::default()
                },
            )
            .unwrap_err();
            assert!(
                error.to_string().contains("parser metadata limit"),
                "{error}"
            );
            assert_eq!(consumed.get(), stop);
        }
    }
}

#[test]
fn gzip_source_buffering_is_separate_from_parser_consumption() {
    let mut bytes = header("large", 11, b'0').as_bytes().to_vec();
    bytes.extend_from_slice(&vec![b'x'; 128 * 1024]);
    let mut compressed = gzip(&bytes);
    // Trailing source data makes decoder read-ahead observable without requiring
    // an incompressible or huge fixture. flate2 1.1.10's read::GzDecoder wraps
    // its source in a 32 KiB BufReader (src/bufreader.rs).
    compressed.extend_from_slice(&vec![b'!'; 128 * 1024]);
    let source_count = Cell::new(0);
    let parser_count = Cell::new(0);
    let source = GuardedReader {
        inner: Cursor::new(compressed),
        consumed: &source_count,
        stop: 32 * 1024,
    };
    let decoder = flate2::read::GzDecoder::new(source);
    let error = validate_archive_reader(
        GuardedReader {
            inner: decoder,
            consumed: &parser_count,
            stop: 512,
        },
        ValidationLimits {
            max_entry_size: 10,
            ..Default::default()
        },
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ExtractError::Invalid(ValidationError::EntryTooLarge { size: 11, .. })
    ));
    assert_eq!(parser_count.get(), 512);
    assert_eq!(source_count.get(), 32 * 1024);
}

#[test]
fn exact_resource_boundaries_and_aggregate_overflow() {
    let mut builder = tar::Builder::new(Vec::new());
    for path in ["first", "other"] {
        let mut entry = header(path, 65537, b'0');
        builder
            .append_data(&mut entry, path, vec![b'x'; 65537].as_slice())
            .unwrap();
    }
    let bytes = builder.into_inner().unwrap();
    let limits = ValidationLimits {
        max_entries: 2,
        max_path_bytes: 5,
        max_entry_size: 65537,
        max_total_size: 131074,
    };
    for reader in readers(&bytes) {
        validate_archive_reader(reader, limits).unwrap();
    }
    for limits in [
        ValidationLimits {
            max_entries: 1,
            ..limits
        },
        ValidationLimits {
            max_path_bytes: 4,
            ..limits
        },
        ValidationLimits {
            max_entry_size: 65536,
            ..limits
        },
        ValidationLimits {
            max_total_size: 131073,
            ..limits
        },
        ValidationLimits {
            max_entries: 0,
            ..limits
        },
        ValidationLimits {
            max_entry_size: 0,
            ..limits
        },
        ValidationLimits {
            max_total_size: 0,
            ..limits
        },
    ] {
        for reader in readers(&bytes) {
            assert!(matches!(
                validate_archive_reader(reader, limits),
                Err(ExtractError::Invalid(_))
            ));
        }
    }
    let entries = [
        ArchiveEntry {
            path: "a".into(),
            kind: EntryKind::File,
            size: u64::MAX,
            link_target: None,
        },
        ArchiveEntry {
            path: "b".into(),
            kind: EntryKind::File,
            size: 1,
            link_target: None,
        },
    ];
    assert_eq!(
        validate_entries(
            entries,
            ValidationLimits {
                max_entry_size: u64::MAX,
                max_total_size: u64::MAX,
                ..Default::default()
            }
        ),
        Err(ValidationError::TotalSizeExceeded)
    );
}

#[test]
fn leading_preflight_stays_within_its_budget_for_repeated_records() {
    for kind in b"LKx" {
        let mut bytes = Vec::new();
        for _ in 0..80 {
            bytes.extend_from_slice(header("metadata", 1024, *kind).as_bytes());
            bytes.extend_from_slice(&[0; 1024]);
        }
        for reader in readers(&bytes) {
            let consumed = Cell::new(0);
            // Preflight deliberately defers duplicate/malformed-record errors
            // to the complete logical pass, but must still bound its own work.
            validate_initial_metadata_reader(
                GuardedReader {
                    inner: reader,
                    consumed: &consumed,
                    stop: 65536,
                },
                ValidationLimits::default(),
            )
            .unwrap();
            assert_eq!(consumed.get(), 65536);
        }
    }
}

#[test]
fn metadata_boundary_includes_extension_and_logical_headers() {
    let project = tapid_test_support::TempProject::new("metadata-boundary").unwrap();
    for extra_record in [false, true] {
        // 512-byte extension header + 64512-byte body + 512-byte file header
        // fits exactly. Another 16-byte PAX record requires another tar block.
        let body = b"16 comment=test\n".repeat(if extra_record { 4033 } else { 4032 });
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append(
                &header("metadata", body.len() as u64, b'x'),
                body.as_slice(),
            )
            .unwrap();
        builder
            .append(&header("file", 0, b'0'), io::empty())
            .unwrap();
        let bytes = builder.into_inner().unwrap();
        for (input, format) in [
            (bytes.clone(), ArchiveFormat::Tar),
            (gzip(&bytes), ArchiveFormat::TarGz),
        ] {
            let destination = project.path().join("tree");
            let result = extract_to(&input, format, &destination, ArchiveLimits::default());
            if extra_record {
                assert!(
                    result
                        .unwrap_err()
                        .to_string()
                        .contains("parser metadata limit")
                );
                assert!(!destination.exists());
            } else {
                result.unwrap();
                assert_eq!(fs::read(destination.join("file")).unwrap(), b"");
                fs::remove_dir_all(destination).unwrap();
            }
        }
    }
}
