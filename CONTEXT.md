# Tapid package installation

Tapid installs a project's selected package graph and retains the evidence needed
to reproduce it. These terms distinguish selection changes from access to package
content.

## Language

### Source identity

The origin of selected package content, including its immutable artifact pin
when copied from a file or repository. Equal package names and versions may have
different source identities.

### Copied artifact

Package content copied from a contained file archive or a repository commit and
identified by an accepted immutable artifact pin.

### Dist-tag

A mutable registry label naming a concrete package version. Its accepted target
becomes a locked selection when the project resolves the declaration.

### Locked selection

An exact package identity and its dependency, peer-provider, and target contexts
already selected for the project. Compatible locked selections remain preferred
when project declarations change.

### Frozen installation

Installation of the existing locked graph with matching project declarations and
no selection changes. Frozen installation permits retrieval of missing pinned content
and records real verification receipts when a selection was persisted without them.

### Offline installation

Installation using locally verified content with no network retrieval. Offline
permission is independent of whether installation is frozen.

### Hydration

Retrieval and verification of missing content for an existing locked selection.
Hydration leaves package selection unchanged.

### Lockfile-only selection

Persistence of a selected graph and requested declaration edits without changing
the installed application. Native schema 11 records missing verification explicitly;
a later frozen installation hydrates exact pins and completes receipts. ci/offline
require completed native receipts. Registry metadata access is permitted during
selection; artifact retrieval is deferred, except for copied-source preparation.
Dry runs remain observations and persist nothing. See the
[CLI effects contract](crates/tapid-cli/README.md) and
[receipt/transaction decision](docs/adr/0012-lockfile-only-selections.md).
