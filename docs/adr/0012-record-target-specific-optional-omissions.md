# Record target-specific optional declaration omissions

An absent optional edge does not explain whether metadata was unavailable, a
candidate was incompatible, or the declaration was never considered. Ordinary
schema 11 locks therefore retain accepted declaration cuts, selected declaration
provenance, and the effective OS/CPU/libc and install profile. Matching replay
preserves these decisions even after metadata recovers or a store becomes cold.
This extends [locked selection and hydration](0008-locked-install-selection-and-hydration.md)
without making network availability part of graph selection during replay.

Cuts identify the exact root, workspace member key, or registry package key,
original local name and requirement, and configured registry origin. Required
reachability takes precedence over optional declarations, including aliases and
workspace members. Selected edges keep separate required and optional declaration
records. Workspace membership roots are distinct from explicit declarations:
all members remain roots, while local links retain their actual manifest sections
and supported workspace protocol symbols. Registry fallback cannot satisfy a
workspace protocol declaration.

The implemented omission policy is deliberately narrow:

| Failure stage | Decision |
| --- | --- |
| Confirmed metadata absence | May omit an optional-only declaration |
| Incompatible platform | May omit an optional-only declaration |
| No usable matching metadata candidate | May omit an optional-only declaration |
| Metadata transport, authentication or download failure | Abort |
| Malformed archive, integrity or package identity mismatch | Abort |
| Unsupported direct specification | Reject during declaration validation |
| Unsupported metadata dependency specification | Candidate normalization may exclude that candidate; required unavailability remains fatal |
| Approved hook failure or containment failure | Abort; no accepted omission reason exists |

This chooses reproducible graph cuts over retrying omitted branches on each
install. Selected optional artifacts receive the same verification as required
artifacts. Human and versioned JSON results expose recorded omissions and bound
their details explicitly. The lockfile capability validates facts; the CLI
validates current declarations and composes verified storage and project
transactions. Neither an optional flag nor a warm cache relaxes verification.

Imported schema 12 uses a separate placement-keyed ledger because ordinary keys
cannot preserve npm placement identity. Normal import generation still emits
schema 8. Copied-source ledger generation, complete imported generation, and
broader reporting remain separate implementation work. Workspace overrides with
omissions currently fail before publication: one requirement field cannot
preserve both the original declaration and its effective override. No support
claim for complete npm optional-failure semantics follows from these formats;
pinned npm comparisons and expanded recovery evidence remain required.
