# Immutable copied dependency sources

Project-root file tarballs and credential-free HTTPS Git dependencies resolve to
copied artifacts. Their source identity includes archive SHA-256; Git identities
also include the peeled full commit. Mutable references remain declarations,
never replay identities. Npm dist-tags likewise resolve to concrete versions
and replay the accepted graph without consulting mutable tags.

Shared package identities let copied artifacts use the existing lock graph and
install transactions. Keeping HTTP inputs as registry origins prevents copied
sources from using registry credentials or registry fallback. Archive digests
identify accepted bytes without asserting registry authentication.

`PackageSource` extends the existing exact package identity used by the resolver,
linker, and protocol. `RegistryOrigin` remains the narrower HTTP registry input.
Schema 10 adds copied source keys and records SHA-512 archive integrity and
verified tree digests through the existing lock model. Copied evidence does not
assert registry authentication. Older schemas reject copied entries.

The CLI prepares copied archive metadata, then uses the existing graph resolver,
safe extraction, store transaction, managed activation, and bin planner. File
archives are contained regular files; replay verifies them against their pin.
Git transport belongs to the registry-client capability and isolates Git from
inherited credentials, configuration, hooks, and non-HTTPS protocols. It fetches
one reference, peels it, rejects submodules, and archives the exact commit.
No checkout or package preparation code runs.

Matching installs retain accepted pins. Explicit updates refresh mutable Git
references and npm tags. A changed root manifest resolves Git declarations again
because the old lock does not retain their original mutable reference spelling.
Frozen and CI cold replay retrieve only pinned artifacts and verify both archive
and tree identity. Offline uses verified trees and still validates file pins.

Workspace-member copies, transitive copied declarations, directory links,
authenticated Git repositories, Git semver selectors, preparation hooks, and
copied-package lifecycle approvals remain unsupported. These require separate
contracts for relative paths, authority, or execution provenance.
