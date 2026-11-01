# Persist selections separately from verified source trees and activation

Repository automation needs to commit dependency selections while an installed
application continues running. `--lockfile-only` resolves through the existing
graph capability once, preserves compatible preferences and save rules, and
publishes that result through the project coordinator. Calling installation and
skipping linking would already have fetched artifacts, initialized the store,
and potentially executed hooks, violating this operation's effects contract.

Native schema 11 separates selection evidence from a required nullable
`sourceTreeReceipt`. A receipt contains both unpacked and replay tree digests and
binds the source-qualified instance, artifact URL, integrity, and provenance.
Absence is pending verification, never a placeholder digest. Legacy schemas keep
their required evidence. Source receipts may be retained only when their binding
matches; they do not authorize derived lifecycle output reuse. Hydration verifies
exact pins and attaches real evidence without downgrading the schema. Complete
native frozen replay remains byte-stable; pending native frozen hydration can
change receipt bytes while preserving selections. This extends the
[earlier locked-install decision](0008-locked-install-selection-and-hydration.md).

Journal version 4 distinguishes Activation from Metadata decisions. Metadata
records only bounded, canonical manifest/lock before/after outputs. Before its
durable decision, abandonment preserves prior outputs; afterward, recovery finishes
publication. Recovery validates all outputs before changing any and refuses
unexpected bytes or redirected paths. Metadata recovery never enters store or
layout recovery; store coordinators accept only activation decisions. Legacy
versions keep their activation semantics. This reuses one coordinator and provides
recoverability across files without promising simultaneous multi-file visibility.

Selection settles metadata recovery before planning, rejects activation recovery,
and rechecks captured inputs under exclusion before publication. Dry runs refuse
pending recovery and remain read-only. ci/offline preflight inspects both visible
lock bytes and the lock an interrupted decision would publish, repeats that check
under exclusion, and rejects pending evidence before recovery effects. Frozen
hydration keeps receipt publication and activation in one recoverable transaction.

Matching imported schema 8 stays imported and preserves placements and receipts;
explicit dependency edits deliberately use native resolution. Its placement and
platform contracts remain distinct from native selection. Registry selection
permits metadata requests only; copied archives and Git pins require their existing
source validation in disposable staging. No new resolver or store coordinator is
introduced. Older readers reject the new schema/journal rather than misinterpreting
unverified content or partially recovering project state.
