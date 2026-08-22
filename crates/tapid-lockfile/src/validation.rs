use crate::LockfileError;

pub(crate) fn validate_url(value: &str) -> Result<(), LockfileError> {
    if value.starts_with("https://")
        && !value.contains('@')
        && !value.contains('?')
        && !value.contains('#')
    {
        Ok(())
    } else {
        Err(LockfileError::InvalidUrl(value.to_owned()))
    }
}

pub(crate) fn validate_sha512(value: &str) -> Result<(), LockfileError> {
    let Some(encoded) = value.strip_prefix("sha512-") else {
        return Err(LockfileError::InvalidSha512(value.to_owned()));
    };
    let valid_length = encoded.len() == 86 || encoded.len() == 88;
    let valid_characters = encoded
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='));
    if !valid_length || !valid_characters || (encoded.contains('=') && !encoded.ends_with("==")) {
        return Err(LockfileError::InvalidSha512(value.to_owned()));
    }
    Ok(())
}
