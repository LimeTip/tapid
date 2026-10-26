//! Streaming structural accounting before typed validation and graph allocation.
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};

use super::{MAX_INSTANCES, MAX_RELATIONSHIPS, SnapshotError};

#[derive(Clone, Copy)]
enum Role {
    Root,
    Instances,
    Instance,
    Relationships,
    Other,
}

#[derive(Default)]
struct Counts {
    instances: usize,
    relationships: usize,
    exceeded: bool,
}

pub(super) fn check(bytes: &[u8]) -> Result<(), SnapshotError> {
    let mut counts = Counts::default();
    let mut parser = serde_json::Deserializer::from_slice(bytes);
    let result = Node {
        counts: &mut counts,
        role: Role::Root,
        depth: 0,
    }
    .deserialize(&mut parser);
    if counts.exceeded {
        return Err(SnapshotError::AnalysisLimit);
    }
    result.map_err(|_| SnapshotError::InvalidSnapshot)?;
    parser.end().map_err(|_| SnapshotError::InvalidSnapshot)
}

struct Node<'a> {
    counts: &'a mut Counts,
    role: Role,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for Node<'_> {
    type Value = ();
    fn deserialize<D: de::Deserializer<'de>>(self, parser: D) -> Result<(), D::Error> {
        if self.depth >= 128 {
            return Err(de::Error::custom("snapshot nesting limit"));
        }
        parser.deserialize_any(self)
    }
}

impl Node<'_> {
    fn count<E: de::Error>(&mut self) -> Result<(), E> {
        match self.role {
            Role::Instances => self.counts.instances += 1,
            Role::Relationships => self.counts.relationships += 1,
            _ => {}
        }
        if self.counts.instances > MAX_INSTANCES || self.counts.relationships > MAX_RELATIONSHIPS {
            self.counts.exceeded = true;
            return Err(E::custom("snapshot analysis limit"));
        }
        Ok(())
    }
}

impl<'de> Visitor<'de> for Node<'_> {
    type Value = ();
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("bounded snapshot JSON")
    }
    fn visit_map<M: MapAccess<'de>>(mut self, mut map: M) -> Result<(), M::Error> {
        while let Some(key) = map.next_key::<String>()? {
            self.count()?;
            let role = match (self.role, key.as_str()) {
                (Role::Root, "npmLock") => Role::Root,
                (Role::Root, "packages" | "workspacePackages") => Role::Instances,
                (Role::Root, "roots" | "rootBindings") => Role::Relationships,
                (Role::Instances, _) => Role::Instance,
                (Role::Instance, "dependencies" | "optionalDependencies" | "peerDependencies") => {
                    Role::Relationships
                }
                _ => Role::Other,
            };
            map.next_value_seed(Node {
                counts: self.counts,
                role,
                depth: self.depth + 1,
            })?;
        }
        Ok(())
    }
    fn visit_seq<S: SeqAccess<'de>>(mut self, mut seq: S) -> Result<(), S::Error> {
        while seq
            .next_element_seed(Node {
                counts: self.counts,
                role: Role::Other,
                depth: self.depth + 1,
            })?
            .is_some()
        {
            self.count()?;
        }
        Ok(())
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<(), E> {
        Ok(())
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<(), E> {
        Ok(())
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<(), E> {
        Ok(())
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<(), E> {
        Ok(())
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<(), E> {
        Ok(())
    }
    fn visit_unit<E: de::Error>(self) -> Result<(), E> {
        Ok(())
    }
}
