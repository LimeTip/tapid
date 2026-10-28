# Contained directory snapshots

Local directory dependencies use copied snapshots; existing workspace links
remain the live editing workflow. Arbitrary live links and external paths require
a separate filesystem authority contract. A snapshot passes through ordinary
archive verification, store publication, and project transactions, rather than
installing a mutable directory as a trusted tree.

The accepted identity is
`directory-v1:<normalized-project-relative-path>#<artifact-sha256>`.
Origin remains significant even when bytes, name, and version match. Schema 11
distinguishes these identities from schema 10 copied file/Git sources without
changing their existing spelling. SHA-256 pins canonical uncompressed GNU tar
bytes; archive SHA-512 and verified tree digests retain their separate roles.

Version one includes regular files, dotfiles, empty directories, and build
outputs, with a regular root `package.json` limited to 1 MiB. Real directories
named `.git` and `node_modules` are excluded at every depth. Ignore files, npm
`files`, and packaging hooks do not alter the view. Entries use portable UTF-8
names in byte order, zero ownership and timestamps, directory mode 0755, and
file mode 0644/0755 according to Unix executable bits. Windows captures ordinary
files as non-executable. GNU long-name records extend paths where necessary;
two zero blocks terminate the archive. Existing archive entry/path/payload limits
and the final 512 MiB ceiling apply. Timestamp-only changes preserve identity.

Traversal must use held filesystem objects and no-follow operations. Reject
links/reparse points, special or multiply linked files, reserved verification
metadata, managed trees, unsafe names, and escaping paths. Validate excluded
entries before skipping them. Detect observable replacement and metadata changes.
This is not an atomic filesystem snapshot: stop writers for a coherent view;
same-length concurrent writes can evade checks or yield a mixed captured view.

The CLI integration must resolve paths from each declaring manifest inside the
explicit project root, reject sources containing the declaring root and distinct
overlapping directory roots, and share repeated captures per operation. Matching,
offline, frozen, and CI replay must recapture even with a warm store. Only an
explicit selected update accepts new content. Member/name/kind-qualified bindings
must preserve unselected pins; offline updates may use only locally available
verified graph content. Transitive copied declarations and copied peer specifiers
remain unsupported. No implicit hooks or extra script filesystem grants follow
from snapshotting or linking.

Implementation is incomplete: current changes establish Unix archive capture and
source/schema representation. Windows capture, additional directory/ancestor race tests, qualified
member bindings, CLI integration, update/replay, and transaction/reporting coverage
must complete before the CLI advertises directory dependency support.
