use crate::ManifestError;

/// Discovers dependency installation and preparation hooks in execution order.
/// Only the script map is validated; unrelated metadata is not interpreted.
pub fn dependency_lifecycle_hooks(input: &str) -> Result<Vec<String>, ManifestError> {
    Ok(dependency_lifecycle_commands(input, false)?
        .into_iter()
        .map(|(hook, _)| hook)
        .collect())
}

/// Returns exact command bytes, including npm's implicit node-gyp install hook.
pub fn dependency_lifecycle_commands(
    input: &str,
    binding_gyp: bool,
) -> Result<Vec<(String, String)>, ManifestError> {
    let value: serde_json::Value =
        serde_json::from_str(input).map_err(ManifestError::InvalidJson)?;
    let object = value.as_object().ok_or(ManifestError::RootMustBeObject)?;
    let empty = serde_json::Map::new();
    let scripts = match object.get("scripts") {
        Some(scripts) => scripts
            .as_object()
            .ok_or(ManifestError::ExpectedMap("scripts"))?,
        None => &empty,
    };
    for (key, value) in scripts {
        if !value.is_string() {
            return Err(ManifestError::ExpectedMapValueString {
                field: "scripts",
                key: key.clone(),
            });
        }
    }
    let mut commands = [
        "preinstall",
        "install",
        "postinstall",
        "prepublish",
        "preprepare",
        "prepare",
        "postprepare",
    ]
    .into_iter()
    .filter(|name| {
        scripts
            .get(*name)
            .and_then(|value| value.as_str())
            .is_some_and(|command| !command.trim().is_empty())
    })
    .map(|name| (name.to_owned(), scripts[name].as_str().unwrap().to_owned()))
    .collect::<Vec<_>>();
    if binding_gyp && !scripts.contains_key("install") && !scripts.contains_key("preinstall") {
        commands.insert(0, ("install".into(), "node-gyp rebuild".into()));
    }
    Ok(commands)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_commands_preserve_bytes_and_phase_order() {
        let input = r#"{"scripts":{"postinstall":"last","install":"  exact\ncommand  ","preinstall":"first","prepare":"unsupported","test":"ignored"}}"#;
        assert_eq!(
            dependency_lifecycle_commands(input, true).unwrap(),
            vec![
                ("preinstall".into(), "first".into()),
                ("install".into(), "  exact\ncommand  ".into()),
                ("postinstall".into(), "last".into()),
                ("prepare".into(), "unsupported".into())
            ]
        );
        assert!(dependency_lifecycle_commands(r#"{"scripts":{"install":42}}"#, false).is_err());
    }
    #[test]
    fn binding_gyp_discovers_implicit_install_without_overriding_explicit_hooks() {
        assert_eq!(
            dependency_lifecycle_commands("{}", true).unwrap(),
            vec![("install".into(), "node-gyp rebuild".into())]
        );
        assert!(
            dependency_lifecycle_commands("{}", false)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            dependency_lifecycle_commands(r#"{"scripts":{"preinstall":"custom"}}"#, true)
                .unwrap()
                .len(),
            1
        );
    }
}
