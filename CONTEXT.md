# Tapid package installation

Tapid installs a project's selected package graph and retains the evidence needed
to reproduce it. These terms distinguish selection changes from access to package
content.

## Language

### Locked selection

An exact package identity and its dependency, peer-provider, and target contexts
already selected for the project. Compatible locked selections remain preferred
when project declarations change.

### Frozen installation

Installation of the existing locked graph with matching project declarations and
no lock changes. Frozen installation permits retrieval of missing pinned content.

### Offline installation

Installation using locally verified content with no network retrieval. Offline
permission is independent of whether installation is frozen.

### Hydration

Retrieval and verification of missing content for an existing locked selection.
Hydration leaves package selection unchanged.
