//! File-only dependency edit protocol. Decoding happens before project access.
use serde::{Deserialize, Serialize};
use std::path::Path;

const MAX_BYTES: usize = 1_048_576;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u32,
    edits: Vec<Edit>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "op", rename_all = "lowercase", deny_unknown_fields)]
enum Edit {
    Add {
        section: Section,
        spec: String,
    },
    Remove {
        section: Section,
        name: String,
    },
    Move {
        name: String,
        from: Section,
        to: Section,
    },
}

#[derive(Debug, Deserialize, Serialize)]
enum Section {
    #[serde(rename = "dependencies")]
    Runtime,
    #[serde(rename = "devDependencies")]
    Development,
    #[serde(rename = "optionalDependencies")]
    Optional,
    #[serde(rename = "peerDependencies")]
    Peer,
}

#[derive(Debug)]
struct InputError {
    message: String,
    index: Option<usize>,
}

fn read(path: &Path) -> Result<Request, InputError> {
    let bytes =
        crate::filesystem::read::read_bounded_file(path, MAX_BYTES).map_err(|_| InputError {
            message: "request must be a regular file of at most 1048576 bytes".into(),
            index: None,
        })?;
    use serde::de::DeserializeSeed;
    let index = std::cell::Cell::new(None);
    let mut decoder = serde_json::Deserializer::from_slice(&bytes);
    let request = RequestSeed(&index)
        .deserialize(&mut decoder)
        .and_then(|request| {
            decoder.end()?;
            Ok(request)
        })
        .map_err(|error| InputError {
            message: error.to_string(),
            index: index.get(),
        })?;
    if request.version != 1 || request.edits.is_empty() || request.edits.len() > 256 {
        return Err(InputError {
            message: "request requires version 1 and between 1 and 256 edits".into(),
            index: None,
        });
    }
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tapid_test_support::TempProject;

    #[test]
    fn strict_request_rejects_duplicate_fields_before_touching_pending_state() {
        let project = TempProject::new("edit-request-duplicates").unwrap();
        let journal = b"pending recovery";
        project
            .write(".tapid-lifecycle-journal.json", journal)
            .unwrap();
        for input in [
            r#"{"version":1,"version":1,"edits":[{"op":"remove","section":"dependencies","name":"old"}]}"#,
            r#"{"version":1,"edits":[{"op":"remove","op":"remove","section":"dependencies","name":"old"}]}"#,
            r#"{"version":1,"edits":[{"op":"remove","section":"dependencies","name":"old","name":"new"}]}"#,
        ] {
            let path = project.write("request.json", input.as_bytes()).unwrap();
            assert!(read(&path).unwrap_err().message.contains("duplicate"));
            assert_eq!(
                std::fs::read(project.path().join(".tapid-lifecycle-journal.json")).unwrap(),
                journal
            );
        }
    }
}

#[derive(Debug, clap::Args)]
pub(crate) struct Args {
    /// Regular version-1 JSON file; at most 1 MiB and 256 edits.
    #[arg(long, value_name = "PATH")]
    pub(super) file: std::path::PathBuf,
    #[command(flatten)]
    pub(super) common: super::lifecycle::CommonArgs,
}

impl Section {
    fn kind(&self) -> tapid_manifest::DependencyKind {
        use tapid_manifest::DependencyKind::*;
        match self {
            Self::Runtime => Dependencies,
            Self::Development => DevDependencies,
            Self::Optional => OptionalDependencies,
            Self::Peer => PeerDependencies,
        }
    }
}
impl Edit {
    fn borrowed(&self) -> crate::application::lifecycle::batch::Edit<'_> {
        use crate::application::lifecycle::batch::Edit as Planned;
        match self {
            Self::Add { section, spec } => Planned::Add {
                section: section.kind(),
                spec,
            },
            Self::Remove { section, name } => Planned::Remove {
                section: section.kind(),
                name,
            },
            Self::Move { name, from, to } => Planned::Move {
                name,
                from: from.kind(),
                to: to.kind(),
            },
        }
    }
}

pub(crate) fn run(args: Args, json: bool) -> std::process::ExitCode {
    use crate::application::{
        install, lifecycle,
        outcome::{ErrorKind, OperationFailure, OperationalError},
    };
    #[cfg(test)]
    crate::application::dependency_edit_evaluation::observe(|counts| {
        counts.command_entries += 1;
        counts.batch_entries += 1;
    });
    let request = match read(&args.file) {
        Ok(request) => request,
        Err(error) => {
            let mut failure = OperationalError::new(ErrorKind::InvalidRequest, error.message);
            if let Some(index) = error.index {
                failure = failure.at_edit(index);
            }
            return crate::output::json::failure_or_human(
                &OperationFailure::unchanged(&args.common.project_dir, failure),
                "edit-dependencies",
                json,
            );
        }
    };
    let result = (|| {
        let selection = lifecycle::resolve_workspace(
            &args.common.project_dir,
            args.common.workspace.as_deref(),
        )
        .map_err(|e| OperationFailure::unchanged(&args.common.project_dir, e))?;
        let edits = request.edits.iter().map(Edit::borrowed).collect::<Vec<_>>();
        let updated = lifecycle::batch::plan(&selection, &edits)
            .map_err(|e| OperationFailure::unchanged(&selection.root_dir, e))?;
        install::run(
            install::InstallRequest {
                project_dir: &selection.root_dir,
                target_manifest_path: &selection.manifest_path,
                mutation: install::ManifestMutation::ReplaceManifest(install::ManifestEdit {
                    expected: &selection.manifest,
                    updated: &updated,
                }),
                store_root: args.common.store_dir.as_deref(),
                mode: install::InstallMode::Online,
                registry_fixture: args.common.registry_fixture.as_deref(),
                allow_unverified_registry_artifacts: args
                    .common
                    .allow_unverified_registry_artifacts,
            },
            |event| crate::output::report_progress(event, json),
        )
    })();
    if json {
        return crate::output::json::dependency_edits(
            &result,
            serde_json::to_value(&request.edits).expect("request edits"),
        );
    }
    match result {
        Ok(report) => {
            crate::output::report_warnings(&report.outcome.warnings);
            println!(
                "Applied {} dependency edits ({} packages)",
                request.edits.len(),
                report.package_count
            );
            std::process::ExitCode::SUCCESS
        }
        Err(error) => crate::output::json::failure_or_human(&error, "edit-dependencies", false),
    }
}

// Seeds retain the current input position even when Serde stops at a malformed
// edit. Object fields remain strict derives, so duplicate keys are never lost
// through an intermediate JSON value map.
struct RequestSeed<'a>(&'a std::cell::Cell<Option<usize>>);
impl<'de> serde::de::DeserializeSeed<'de> for RequestSeed<'_> {
    type Value = Request;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Request, D::Error> {
        deserializer.deserialize_map(self)
    }
}
impl<'de> serde::de::Visitor<'de> for RequestSeed<'_> {
    type Value = Request;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a version-1 dependency-edit request")
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Request, A::Error> {
        use serde::de::Error;
        let mut version = None;
        let mut edits = None;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "version" => {
                    if version.is_some() {
                        return Err(A::Error::duplicate_field("version"));
                    }
                    version = Some(map.next_value::<u32>()?);
                }
                "edits" => {
                    if edits.is_some() {
                        return Err(A::Error::duplicate_field("edits"));
                    }
                    edits = Some(map.next_value_seed(EditsSeed(self.0))?);
                }
                _ => return Err(A::Error::unknown_field(&key, &["version", "edits"])),
            }
        }
        Ok(Request {
            version: version.ok_or_else(|| A::Error::missing_field("version"))?,
            edits: edits.ok_or_else(|| A::Error::missing_field("edits"))?,
        })
    }
}
struct EditsSeed<'a>(&'a std::cell::Cell<Option<usize>>);
impl<'de> serde::de::DeserializeSeed<'de> for EditsSeed<'_> {
    type Value = Vec<Edit>;
    fn deserialize<D: serde::Deserializer<'de>>(
        self,
        deserializer: D,
    ) -> Result<Vec<Edit>, D::Error> {
        deserializer.deserialize_seq(self)
    }
}
impl<'de> serde::de::Visitor<'de> for EditsSeed<'_> {
    type Value = Vec<Edit>;
    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("1 to 256 dependency edits")
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<Edit>, A::Error> {
        use serde::de::Error;
        let mut edits = Vec::new();
        loop {
            self.0.set(Some(edits.len()));
            if edits.len() == 256 {
                if seq.next_element::<serde::de::IgnoredAny>()?.is_some() {
                    return Err(A::Error::custom("request exceeds 256 edits"));
                }
                break;
            }
            match seq.next_element::<Edit>()? {
                Some(edit) => edits.push(edit),
                None => break,
            }
        }
        self.0.set(None);
        Ok(edits)
    }
}
