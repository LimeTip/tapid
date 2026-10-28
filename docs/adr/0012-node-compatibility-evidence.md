# ADR 0012: Explicit Node compatibility evidence

Package installation must work offline without Node or package-code execution.
Compatibility therefore composes preserved manifest, registry and lock declarations
with optional explicit executable/version observations. Advisory reporting is the
default; strict activation requires complete evaluable requirements. Reuse the
resolver's range semantics rather than a second engine evaluator or dependency
reselection. Support only exact Tapid manager declarations; never provision or
delegate to Corepack or another manager.

Schema 11 persists known absence, ranges, malformed declarations and unknown
metadata. Historical omission remains unknown rather than becoming an invented
compatibility claim. Imported npm schema 8 keeps its own representation. Recovery
from verified original package manifests enriches an operation report before
hooks; frozen locks are not rewritten. Lock/registry claims retain provenance
separate from verified archive content. This deliberate schema cost preserves
honest offline evidence and the existing lifecycle and copied-source contracts.

Explicit identification excludes project and store paths and clears ambient
startup environment. It reuses the runner's caller-authorized uncontained leader
supervision with a five-second deadline and 4 KiB accepted-output ceiling. This
runs caller-selected external code and provides no containment, publisher
identity or descendant cleanup guarantee. Canonical pathname validation retains
replacement-race limits. Its observation never certifies a different runtime
selected for `tapid run`; runner policy and native identity checks remain separate.

A single report feeds advisory and strict installation, local status and exact
package explanation. Full evaluation precedes bounded human/JSON presentation.
This avoids a competing doctor command and keeps evidence completeness distinct
from policy success and application readiness. Standalone operation needs no Cloud
service, account, runtime download or external compatibility provider.
