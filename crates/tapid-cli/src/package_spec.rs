pub(crate) fn parse(spec: &str) -> (&str, &str) {
    let spec = spec.trim();
    let package_start = if spec.starts_with("npm:") || spec.starts_with("jsr:") {
        4
    } else {
        0
    };
    match spec.rfind('@') {
        Some(position) if position > package_start => {
            let requirement = spec[position + 1..].trim();
            if requirement.is_empty() {
                (spec[..position].trim_end(), "*")
            } else {
                (spec[..position].trim_end(), requirement)
            }
        }
        _ => (spec, "*"),
    }
}

#[cfg(test)]
mod tests {
    use super::parse;

    #[test]
    fn preserves_scoped_names_and_registry_prefixes() {
        assert_eq!(parse("@scope/pkg"), ("@scope/pkg", "*"));
        assert_eq!(parse("@scope/pkg@1.2.3"), ("@scope/pkg", "1.2.3"));
        assert_eq!(parse("jsr:@scope/pkg"), ("jsr:@scope/pkg", "*"));
        assert_eq!(parse("npm:@scope/pkg@1.2.3"), ("npm:@scope/pkg", "1.2.3"));
        assert_eq!(parse(" foo@ 1.2.3 "), ("foo", "1.2.3"));
        assert_eq!(parse("foo@ "), ("foo", "*"));
    }
}
