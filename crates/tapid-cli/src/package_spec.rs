pub(crate) fn parse(spec: &str) -> (&str, &str) {
    let spec = spec.trim();
    for prefix in ["@npm:", "@file:", "@git+"] {
        if let Some((name, target)) = spec.split_once(prefix) {
            let alias_start = name.len() + 1;
            if !name.is_empty() && !target.is_empty() {
                return (name, &spec[alias_start..]);
            }
        }
    }
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
    use proptest::prelude::*;

    #[test]
    fn preserves_scoped_names_and_registry_prefixes() {
        assert_eq!(parse("@scope/pkg"), ("@scope/pkg", "*"));
        assert_eq!(parse("@scope/pkg@1.2.3"), ("@scope/pkg", "1.2.3"));
        assert_eq!(parse("jsr:@scope/pkg"), ("jsr:@scope/pkg", "*"));
        assert_eq!(parse("npm:@scope/pkg@1.2.3"), ("npm:@scope/pkg", "1.2.3"));
        assert_eq!(parse(" foo@ 1.2.3 "), ("foo", "1.2.3"));
        assert_eq!(parse("foo@ "), ("foo", "*"));
    }

    #[test]
    fn preserves_alias_target_and_range_as_one_declaration() {
        assert_eq!(parse("local@npm:actual@^1"), ("local", "npm:actual@^1"));
        assert_eq!(
            parse("@local/pkg@npm:@actual/pkg@1.2.3"),
            ("@local/pkg", "npm:@actual/pkg@1.2.3")
        );
        assert_eq!(parse("local@npm:actual"), ("local", "npm:actual"));
    }

    proptest! {
        #[test]
        fn trims_and_defaults_requirement_for_generated_package_specs(
            prefix in prop::sample::select(vec!["", "npm:", "jsr:"]),
            name in prop::sample::select(vec!["pkg", "@scope/pkg"]),
            requirement in "[A-Za-z0-9.^~><= -]{0,24}",
        ) {
            let package = format!("{prefix}{name}");
            let spec = format!("  {package}@ {requirement}  ");
            let expected = if requirement.trim().is_empty() { "*" } else { requirement.trim() };
            prop_assert_eq!(parse(&spec), (package.as_str(), expected));
        }

        #[test]
        fn arbitrary_specs_without_at_are_trimmed_and_unconstrained(value in "[^@\\r\\n]{0,64}") {
            let wrapped = format!("  {value}  ");
            prop_assert_eq!(parse(&wrapped), (value.trim(), "*"));
        }
    }
}
