//! Explicit snapshot I/O only. Never enter lifecycle application orchestration.
use std::{path::PathBuf, process::ExitCode};

use clap::Args;
use serde::Serialize;
use serde_json::json;
use tapid_lockfile::{GraphInstance, GraphRelationship, GraphScope, GraphSnapshot, SnapshotError};

#[derive(Debug, Args)]
pub(crate) struct InspectArgs {
    snapshot: PathBuf,
    #[command(flatten)]
    page: Options,
}

#[derive(Debug, Args)]
pub(crate) struct CompareArgs {
    before: PathBuf,
    after: PathBuf,
    #[command(flatten)]
    page: Options,
}

#[derive(Debug, Args)]
struct Options {
    /// Select actual package names, without dependency-closure expansion (repeatable).
    #[arg(long = "package", conflicts_with = "instance")]
    packages: Vec<String>,
    /// Select exact opaque instance references (repeatable).
    #[arg(long = "instance")]
    instance: Vec<String>,
    /// Maximum detail records in this page. Complete analysis still precedes pagination.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u16).range(1..=1000))]
    limit: u16,
    /// Continue the same snapshots and scope using the prior result's opaque token.
    #[arg(long)]
    cursor: Option<String>,
}

impl Options {
    fn scope(&self) -> GraphScope {
        if !self.packages.is_empty() {
            GraphScope::Packages(self.packages.clone())
        } else if !self.instance.is_empty() {
            GraphScope::Instances(self.instance.clone())
        } else {
            GraphScope::All
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
enum Detail<'a> {
    Instance(&'a GraphInstance),
    Relationship(&'a GraphRelationship),
    AddedInstance(&'a GraphInstance),
    RemovedInstance(&'a GraphInstance),
    RelationshipChange(&'a tapid_lockfile::GraphRelationshipChange),
    EvidenceChange(&'a tapid_lockfile::GraphEvidenceChange),
}

fn read(path: &std::path::Path) -> Result<GraphSnapshot, &'static str> {
    let bytes =
        crate::filesystem::read::read_bounded_file(path, 32 * 1024 * 1024).map_err(|error| {
            match error {
                crate::filesystem::read::ReadError::CapacityExceeded => "LOCK_GRAPH_INPUT_LIMIT",
                crate::filesystem::read::ReadError::Io(_) => "LOCK_GRAPH_IO",
            }
        })?;
    GraphSnapshot::from_bytes(&bytes).map_err(code)
}

fn code(error: SnapshotError) -> &'static str {
    match error {
        SnapshotError::InputLimit => "LOCK_GRAPH_INPUT_LIMIT",
        SnapshotError::AnalysisLimit => "LOCK_GRAPH_ANALYSIS_LIMIT",
        SnapshotError::InvalidSnapshot => "LOCK_GRAPH_INVALID_SNAPSHOT",
        SnapshotError::UnsupportedFormat => "LOCK_GRAPH_UNSUPPORTED_FORMAT",
        SnapshotError::InvalidScope => "LOCK_GRAPH_INVALID_SCOPE",
    }
}

fn failure(code: &str) -> ExitCode {
    crate::output::json::operation_error("lock", code, None)
}

fn require_json(json: bool) -> bool {
    if !json {
        eprintln!("error: lock inspect and lock compare require --json");
    }
    json
}

pub(super) fn inspect(args: InspectArgs, json_output: bool) -> ExitCode {
    if !require_json(json_output) {
        return ExitCode::from(2);
    }
    let result = (|| {
        let snapshot = read(&args.snapshot)?;
        snapshot.inspect_scope(&args.page.scope()).map_err(code)
    })();
    let graph = match result {
        Ok(graph) => graph,
        Err(code) => return failure(code),
    };
    let binding = json!(["inspect", graph.snapshot_digest, graph.scope]).to_string();
    let data = json!({
        "contract_version":1,"action":"inspect","snapshot_digest":graph.snapshot_digest,
        "schema":graph.schema,"scope":graph.scope,"coverage":graph.coverage,
        "availability":graph.availability,"unavailable_reasons":graph.availability.unavailable_reasons(),
        "summary":{"instances":graph.instances.len(),"relationships":graph.relationships.len()},
    });
    let details: Vec<_> = graph
        .instances
        .iter()
        .map(Detail::Instance)
        .chain(graph.relationships.iter().map(Detail::Relationship))
        .collect();
    crate::output::json::lock_graph(
        data,
        &details,
        &binding,
        args.page.cursor.as_deref(),
        args.page.limit.into(),
        true,
    )
}

pub(super) fn compare(args: CompareArgs, json_output: bool) -> ExitCode {
    if !require_json(json_output) {
        return ExitCode::from(2);
    }
    let result = (|| {
        let before = read(&args.before)?;
        let after = read(&args.after)?;
        before
            .compare_scope(&after, &args.page.scope())
            .map_err(code)
    })();
    let graph = match result {
        Ok(graph) => graph,
        Err(code) => return failure(code),
    };
    let binding = json!([
        "compare",
        graph.before_digest,
        graph.after_digest,
        graph.scope
    ])
    .to_string();
    let summary = graph.selected_graph_supported.then(|| json!({
        "added_instances":graph.added_instances.len(),"removed_instances":graph.removed_instances.len(),
        "changed_selections":graph.changed_selections,"relationship_changes":graph.relationship_changes.len(),
        "evidence_changes":graph.evidence_changes.len(),
    }));
    let data = json!({
        "contract_version":1,"action":"compare","before_digest":graph.before_digest,"after_digest":graph.after_digest,
        "scope":graph.scope,"before_coverage":graph.before_coverage,"after_coverage":graph.after_coverage,
        "before_availability":graph.before_availability,"after_availability":graph.after_availability,
        "before_unavailable_reasons":graph.before_availability.unavailable_reasons(),"after_unavailable_reasons":graph.after_availability.unavailable_reasons(),
        "selected_graph_supported":graph.selected_graph_supported,"unsupported_reason":graph.unsupported_reason,
        "summary":summary,
    });
    let details: Vec<_> = graph
        .added_instances
        .iter()
        .map(Detail::AddedInstance)
        .chain(graph.removed_instances.iter().map(Detail::RemovedInstance))
        .chain(
            graph
                .relationship_changes
                .iter()
                .map(Detail::RelationshipChange),
        )
        .chain(graph.evidence_changes.iter().map(Detail::EvidenceChange))
        .collect();
    crate::output::json::lock_graph(
        data,
        &details,
        &binding,
        args.page.cursor.as_deref(),
        args.page.limit.into(),
        graph.selected_graph_supported,
    )
}
