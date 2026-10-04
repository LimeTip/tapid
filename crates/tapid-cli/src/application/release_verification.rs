//! Read-only native signature verification for authenticated installer bootstraps.
use std::{fs::File, io::Read, path::Path};
use tapid_signatures::KeyRing;
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

use super::release_record::{MAX_BYTES, verify_signature};

fn read_bounded(path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take((MAX_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > MAX_BYTES {
        return Err("release verification input exceeds the size limit".into());
    }
    Ok(bytes)
}

pub(crate) fn verify(record: &Path, sidecar: &Path, keyring: Option<&Path>) -> Result<(), String> {
    let keyring = match keyring {
        Some(path) => KeyRing::from_embedded_json(&read_bounded(path)?),
        None => KeyRing::production(),
    }
    .map_err(|e| e.to_string())?;
    let now = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|e| e.to_string())?;
    verify_signature(
        &read_bounded(record)?,
        &read_bounded(sidecar)?,
        &keyring,
        &now,
    )
    .map_err(|e| e.to_string())
}
