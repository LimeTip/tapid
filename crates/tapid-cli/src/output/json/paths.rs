//! Lossless native paths, separate from bounded terminal-safe display strings.
use serde_json::{Value, json};
use std::path::Path;

const MAX_PATH_BYTES: usize = 128 * 1024;

fn unavailable(encoding: &str) -> Value {
    json!({"encoding": encoding, "value": null, "unavailable": "capacity_exceeded"})
}

pub(super) fn encode(path: &Path) -> Value {
    if let Some(path) = path.to_str() {
        return if path.len() <= MAX_PATH_BYTES {
            json!({"encoding": "utf8", "value": path})
        } else {
            unavailable("utf8")
        };
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        encode_native("unix_bytes_base64", path.as_os_str().as_bytes())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let bytes = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        encode_native("windows_utf16le_base64", &bytes)
    }
}

fn encode_native(encoding: &str, bytes: &[u8]) -> Value {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    if bytes.len() > MAX_PATH_BYTES {
        unavailable(encoding)
    } else {
        json!({"encoding": encoding, "value": STANDARD.encode(bytes)})
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unicode_and_control_characters_are_preserved_without_raw_ansi_output() {
        let project = tapid_test_support::TempProject::new("json-native-path").unwrap();
        let path = project.path().join("界\n\u{1b}[31m");
        let encoded = encode(&path);
        assert_eq!(encoded["encoding"], "utf8");
        assert_eq!(encoded["value"], path.to_str().unwrap());
        assert!(!encoded.to_string().contains('\u{1b}'));
    }

    #[test]
    fn capacity_is_explicit_and_never_returns_a_shortened_path() {
        let encoded = encode(Path::new(&"x".repeat(MAX_PATH_BYTES + 1)));
        assert!(encoded["value"].is_null());
        assert_eq!(encoded["unavailable"], "capacity_exceeded");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_unix_paths_round_trip_native_bytes() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use std::{
            ffi::OsString,
            os::unix::ffi::{OsStrExt, OsStringExt},
        };
        let project = tapid_test_support::TempProject::new("json-native-bytes").unwrap();
        let path = project.path().join(OsString::from_vec(vec![b'x', 0xff]));
        let encoded = encode(&path);
        assert_eq!(encoded["encoding"], "unix_bytes_base64");
        assert_eq!(
            STANDARD.decode(encoded["value"].as_str().unwrap()).unwrap(),
            path.as_os_str().as_bytes()
        );
    }

    #[cfg(windows)]
    #[test]
    fn unpaired_windows_surrogates_round_trip_native_units() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use std::{
            ffi::OsString,
            os::windows::ffi::{OsStrExt, OsStringExt},
        };
        let project = tapid_test_support::TempProject::new("json-native-units").unwrap();
        let path = project
            .path()
            .join(OsString::from_wide(&[b'x' as u16, 0xd800]));
        let encoded = encode(&path);
        assert_eq!(encoded["encoding"], "windows_utf16le_base64");
        let expected = path
            .as_os_str()
            .encode_wide()
            .flat_map(u16::to_le_bytes)
            .collect::<Vec<_>>();
        assert_eq!(
            STANDARD.decode(encoded["value"].as_str().unwrap()).unwrap(),
            expected
        );
    }
}
