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
