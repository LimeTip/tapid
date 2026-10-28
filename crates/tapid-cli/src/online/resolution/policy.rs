//! Compile preserved root declarations into the resolver's range-only policy.
use super::*;
use crate::application::outcome::Declaration;
use tapid_manifest::OverrideDeclaration;
use tapid_resolver::OverrideRule;

fn selector(value: &str, qualified: bool) -> Result<(PackageName, Option<Requirement>), String> {
    let value = value.strip_prefix("npm:").unwrap_or(value);
    let split = value.rfind('@').filter(|index| *index > 0);
    let (name, range) = match split {
        Some(index) if qualified => (&value[..index], Some(&value[index + 1..])),
        Some(_) => {
            return Err(format!(
                "unsupported override selector '{value}': child version selectors are not supported"
            ));
        }
        None => (value, None),
    };
    let name = name
        .parse::<PackageName>()
        .map_err(|error| format!("invalid override selector '{value}': {error}"))?;
    let range = range.map(|range| replacement(value, range)).transpose()?;
    Ok((name, range))
}

fn replacement(selector: &str, value: &str) -> Result<Requirement, String> {
    if value.trim().is_empty() {
        return Err(format!(
            "invalid override '{selector}': empty version range"
        ));
    }
    let requirement = value
        .parse::<Requirement>()
        .map_err(|error| format!("invalid override '{selector}' range '{value}': {error}"))?;
    if requirement.is_alias() || requirement.dist_tag().is_some() {
        return Err(format!(
            "unsupported override '{selector}': override values must be version ranges"
        ));
    }
    Ok(requirement)
}

pub(crate) fn compile_override_rules(
    manifest: &PackageManifest,
) -> Result<Vec<OverrideRule>, String> {
    let mut rules = Vec::new();
    for (key, declaration) in manifest.overrides() {
        let pointer = Declaration::pointer("overrides", key);
        match declaration {
            OverrideDeclaration::Replacement(value) => {
                let (dependency, _) = selector(key, false)?;
                rules.push(OverrideRule {
                    id: pointer,
                    parent: None,
                    dependency,
                    replacement: replacement(key, value)?,
                });
            }
            OverrideDeclaration::Nested(children) => {
                let parent = selector(key, true)?;
                for (child, value) in children {
                    let id = format!("{pointer}/{}", child.replace('~', "~0").replace('/', "~1"));
                    let OverrideDeclaration::Replacement(value) = value else {
                        return Err(format!(
                            "unsupported override '{id}': deeper nesting is not supported"
                        ));
                    };
                    let (dependency, _) = selector(child, false)?;
                    rules.push(OverrideRule {
                        id,
                        parent: Some(parent.clone()),
                        dependency,
                        replacement: replacement(child, value)?,
                    });
                }
            }
        }
    }
    Ok(rules)
}
