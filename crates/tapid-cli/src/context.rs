use tapid_core::{PackageName, PackageVersion, PeerContext, PlatformContext};

fn percent_decode(value: &str) -> Result<String, String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(format!("invalid percent encoding: {value}"));
            }
            let high = (bytes[index + 1] as char)
                .to_digit(16)
                .ok_or_else(|| format!("invalid percent encoding: {value}"))?;
            let low = (bytes[index + 2] as char)
                .to_digit(16)
                .ok_or_else(|| format!("invalid percent encoding: {value}"))?;
            decoded.push((high * 16 + low) as u8);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| format!("invalid UTF-8 context: {value}"))
}

pub(crate) fn parse_peer(value: &str) -> Result<PeerContext, String> {
    if value.is_empty() || value == "-" {
        return Ok(PeerContext::default());
    }
    let mut result = PeerContext::default();
    for item in value.split(',') {
        let (name, version) = if item.starts_with("name=") {
            let fields: std::collections::BTreeMap<_, _> = item
                .split(';')
                .map(|field| {
                    field
                        .split_once('=')
                        .ok_or_else(|| format!("invalid peer context: {value}"))
                })
                .collect::<Result<_, _>>()?;
            (
                percent_decode(
                    fields
                        .get("name")
                        .copied()
                        .ok_or_else(|| format!("invalid peer context: {value}"))?,
                )?,
                percent_decode(
                    fields
                        .get("version")
                        .copied()
                        .ok_or_else(|| format!("invalid peer context: {value}"))?,
                )?,
            )
        } else {
            let (name, version) = item
                .rsplit_once('@')
                .ok_or_else(|| format!("invalid peer context: {value}"))?;
            (name.to_owned(), version.to_owned())
        };
        result = result.with(
            name.parse::<PackageName>().map_err(|e| e.to_string())?,
            version
                .parse::<PackageVersion>()
                .map_err(|e| e.to_string())?,
        );
    }
    Ok(result)
}

pub(crate) fn parse_platform(value: &str) -> Result<PlatformContext, String> {
    if value.is_empty() || value == "-" {
        return PlatformContext::new(None, None, None).map_err(|e| e.to_string());
    }
    if value.starts_with("os=") {
        let fields: std::collections::BTreeMap<_, _> = value
            .split(';')
            .map(|field| {
                field
                    .split_once('=')
                    .ok_or_else(|| format!("invalid platform context: {value}"))
            })
            .collect::<Result<_, _>>()?;
        let decode_optional = |key: &str| -> Result<Option<String>, String> {
            let raw = fields
                .get(key)
                .copied()
                .ok_or_else(|| format!("invalid platform context: {value}"))?;
            if raw.is_empty() {
                Ok(None)
            } else {
                percent_decode(raw).map(Some)
            }
        };
        let os = decode_optional("os")?;
        let cpu = decode_optional("cpu")?;
        let libc = decode_optional("libc")?;
        return PlatformContext::new(os.as_deref(), cpu.as_deref(), libc.as_deref())
            .map_err(|e| e.to_string());
    }
    let parts: Vec<_> = value.split('-').collect();
    PlatformContext::new(
        parts.first().copied(),
        parts.get(1).copied(),
        parts.get(2).copied(),
    )
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_canonical_peer_context_with_percent_encoding() {
        let parsed = parse_peer("name=%40scope%2Fpkg;version=1.2.3").unwrap();
        let (name, version) = parsed.entries().iter().next().unwrap();
        assert_eq!(name.as_str(), "@scope/pkg");
        assert_eq!(version.to_string(), "1.2.3");
    }

    #[test]
    fn parses_canonical_platform_context_with_percent_encoding() {
        let parsed = parse_platform("os=linux;cpu=x86%2D64;libc=gnu").unwrap();
        assert_eq!(parsed.os.as_deref(), Some("linux"));
        assert_eq!(parsed.cpu.as_deref(), Some("x86-64"));
        assert_eq!(parsed.libc.as_deref(), Some("gnu"));
    }

    #[test]
    fn parses_empty_canonical_platform_context() {
        let parsed = parse_platform("os=;cpu=;libc=").unwrap();
        assert_eq!(parsed.os, None);
        assert_eq!(parsed.cpu, None);
        assert_eq!(parsed.libc, None);
    }
}
