//! Source-level override checks that must run before duplicate evidence is lost.

use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, Visitor},
};
use std::{collections::BTreeSet, fmt};

pub(crate) fn validate_unique_keys(input: &str) -> Result<(), serde_json::Error> {
    #[derive(Deserialize)]
    struct Document {
        #[serde(default, rename = "overrides")]
        _overrides: Option<UniqueDeclaration>,
    }
    serde_json::from_str::<Document>(input).map(|_| ())
}

struct UniqueDeclaration;

impl<'de> Deserialize<'de> for UniqueDeclaration {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct DeclarationVisitor;
        impl<'de> Visitor<'de> for DeclarationVisitor {
            type Value = UniqueDeclaration;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an override string or object")
            }

            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(UniqueDeclaration)
            }

            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut keys = BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key.clone()) {
                        return Err(de::Error::custom(format!("duplicate override key '{key}'")));
                    }
                    map.next_value::<UniqueDeclaration>()?;
                }
                Ok(UniqueDeclaration)
            }
        }
        deserializer.deserialize_any(DeclarationVisitor)
    }
}

/// Parse a local override binding and, for a parent, its optional range text.
/// Range semantics are compiled by the resolver consumer. This does not activate
/// declarations preserved from package manifests.
pub fn parse_override_selector(
    value: &str,
    allow_parent_range: bool,
) -> Result<(tapid_core::PackageName, Option<&str>), String> {
    let value = value.strip_prefix("npm:").unwrap_or(value);
    let split = value.rfind('@').filter(|index| *index > 0);
    let (name, range) = match split {
        Some(index) if allow_parent_range => (&value[..index], Some(&value[index + 1..])),
        Some(_) => {
            return Err(format!(
                "unsupported override selector '{value}': child version selectors are not supported"
            ));
        }
        None => (value, None),
    };
    let name = name
        .parse()
        .map_err(|error| format!("invalid override selector '{value}': {error}"))?;
    Ok((name, range))
}
