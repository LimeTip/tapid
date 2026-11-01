use crate::{ManifestError, PackageManifest};
use serde_json::Value;

/// Borrowed packaging declarations. Filesystem and pattern policy belong to the packer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackagingMetadata<'a> {
    pub files: Option<Vec<&'a str>>,
    pub main: Option<&'a str>,
}

impl PackageManifest {
    /// Validate packaging declarations on demand without changing ordinary parsing.
    /// This does not validate selection patterns or inspect declared filesystem targets.
    pub fn packaging(&self) -> Result<PackagingMetadata<'_>, ManifestError> {
        let files = self
            .extra
            .get("files")
            .map(|value| {
                value
                    .as_array()
                    .ok_or(ManifestError::InvalidPackaging {
                        field: "files",
                        expected: "array_of_strings",
                    })?
                    .iter()
                    .map(|entry| {
                        entry.as_str().ok_or(ManifestError::InvalidPackaging {
                            field: "files",
                            expected: "array_of_strings",
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?;
        let main = self
            .extra
            .get("main")
            .map(|value| {
                value
                    .as_str()
                    .filter(|target| crate::parse::valid_target(target))
                    .ok_or(ManifestError::InvalidPackaging {
                        field: "main",
                        expected: "relative_file_target",
                    })
            })
            .transpose()?;
        for field in ["bundleDependencies", "bundledDependencies"] {
            if let Some(value) = self.extra.get(field)
                && !matches!(value, Value::Bool(false))
                && !matches!(value, Value::Array(entries) if entries.is_empty())
            {
                return Err(ManifestError::InvalidPackaging {
                    field,
                    expected: "false_or_empty_array",
                });
            }
        }
        if let Some(value) = self.extra.get("directories") {
            let object = value
                .as_object()
                .ok_or(ManifestError::ExpectedMap("directories"))?;
            if object.contains_key("bin") {
                return Err(ManifestError::InvalidPackaging {
                    field: "directories",
                    expected: "object_without_bin",
                });
            }
        }
        Ok(PackagingMetadata { files, main })
    }
}
