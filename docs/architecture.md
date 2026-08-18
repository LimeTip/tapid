# Tapid Architecture

Tapid starts as a Rust workspace in one public repository. The workspace provides separation between product responsibilities without splitting the contributor experience across multiple repositories.

## Initial crates

```text
crates/
├── tapid-cli
├── tapid-core
├── tapid-store
└── tapid-manifest
```

`tapid-cli` owns command-line parsing, terminal output, and process exit behavior.

`tapid-core` owns pure package-manager domain types and deterministic validation. It must not depend on the CLI, network, operating system, or a particular registry implementation.

`tapid-store` owns the local content-addressed package store. It is not the online package registry. It will eventually handle verified local artifacts, offline reuse, deduplication, quarantine, and conservative garbage collection.

## Dependency direction

```text
tapid-cli -> tapid-core
tapid-cli -> tapid-manifest
tapid-cli -> tapid-store
tapid-manifest -> tapid-core
tapid-store -> tapid-core
```

Future crates may include lockfiles, resolution, registry clients, archive validation, linking, policy, execution, publishing, protocol, signatures, attestations, and transparency support. The manifest crate now owns parsing and validation for the supported `package.json` fields, while filesystem mutation remains in the CLI. Other planned boundaries should gain real behavior incrementally, not remain empty placeholders.

## Registry boundary

The future Tapid Registry is an online service for package metadata, publishing, distribution, identities, and attestations. It is distinct from `tapid-store`, which is local developer-machine storage.

The main repository remains the default home for the client and its libraries. A future registry service or independently operated storage system may use a separate repository when its deployment, security, ownership, or release lifecycle justifies that split.