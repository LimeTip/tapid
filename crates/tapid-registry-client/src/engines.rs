//! Preserved Node requirements from a registry version's metadata.
use serde_json::Value;

/// Declaration evidence, without range evaluation or a runtime compatibility claim.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum NodeEngineRequirement {
    /// This source did not provide engine evidence.
    #[default]
    Unknown,
    /// Inspected metadata contains no Node declaration.
    NotDeclared,
    /// Preserved string; validity is determined by the resolver's range contract.
    Range(String),
    /// The declared engines field is not an object.
    MalformedEngines,
    /// The declared engines.node field is not a string.
    MalformedNode,
}

impl NodeEngineRequirement {
    /// Extract from an inspected engines field, retaining malformed declarations.
    pub fn from_engines(engines: Option<&Value>) -> Self {
        match engines {
            None => Self::NotDeclared,
            Some(Value::Object(fields)) => match fields.get("node") {
                None => Self::NotDeclared,
                Some(Value::String(range)) => Self::Range(range.clone()),
                Some(_) => Self::MalformedNode,
            },
            Some(_) => Self::MalformedEngines,
        }
    }
}
