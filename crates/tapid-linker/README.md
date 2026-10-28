# tapid-linker

`ShimStrategy::validate_targets` checks generated output identities using the same
platform collision rules as shim planning, without inspecting bin targets or
selecting interpreters. Callers remain responsible for path and execution policy.

[![CI](https://github.com/LimeTip/tapid/actions/workflows/ci.yml/badge.svg)](https://github.com/LimeTip/tapid/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/tapid-linker)](https://crates.io/crates/tapid-linker)
[![Crates.io downloads](https://img.shields.io/crates/d/tapid-linker)](https://crates.io/crates/tapid-linker)
[![Docs.rs](https://docs.rs/tapid-linker/badge.svg)](https://docs.rs/tapid-linker)
[![License](https://img.shields.io/badge/license-Apache--2.0-blue)](https://github.com/LimeTip/tapid/blob/main/LICENSE)
[![Rust 1.88+](https://img.shields.io/badge/rust-1.88%2B-000000?logo=rust&logoColor=white)](https://www.rust-lang.org/)

`tapid-linker` owns deterministic planning for materializing verified package instances and executable shims into a project layout. It accepts source-qualified identities, peer and platform context, verified content-addressed tree references, and package metadata. Sources may be registry origins or immutable copied file/Git identities. Plans are sorted and constrained to an explicitly managed absolute root with a `.tapid-managed` marker path.

`plan_layout` places root dependencies at `<project>/node_modules/<name>` and nested edges under the parent package's `node_modules`. `InstanceKey` includes package identity plus peer and platform context. Instance storage paths include a SHA-256 digest of the canonical source identity, so registry and copied packages cannot share an artifact directory based only on name and version. Cycle planning tracks dependency lookup at each placement, so returning instances under aliases keep their locked dependencies. Cycles requiring unbounded nested copies fail with a conflicting-target error. Conflicting targets, duplicate instances, unknown edges, escaping paths, unsupported platforms, malformed `bin` metadata, missing targets, symlinks, special files, and shim collisions are rejected.

`plan_workspace_layout` uses the named registry planner while reserving root workspace-link targets. Registry dependencies with the same install name stay beneath their registry parent instead of replacing a local link. The CLI invokes this planner separately for the root and each managed member, retaining every direct registry binding within a member layout.

`plan_shims` selects Unix symlinks or Windows command and PowerShell wrappers. The planner validates that each bin target is a regular file inside its verified tree. Windows plans identify supported Node shebangs from a bounded 512-byte prefix and emit `ShimInterpreter::Node`; native targets without a shebang use `Direct`. Unsupported shebang interpreters or arguments reject before shim writes. Both Windows wrapper formats invoke Node for Node scripts, preserve forwarded arguments, and propagate the child exit code. The CLI materializes the plan during staged install and atomically activates the managed layout.

Windows collisions are checked against the actual output paths produced by replacing the command's extension with `.cmd` and `.ps1`: `Tool`/`tool`, `tool`/`tool.cmd`, and `tool.cmd`/`tool.ps1` must not compete for those outputs. Comparison uses non-expanding BMP uppercase mappings over UTF-16 units; surrogate units remain unchanged. This catches ASCII and tested Unicode cases (including sigma variants), but Rust's Unicode mapping is not a query of a Windows volume's upcase table. It is not a universal NTFS, case-sensitive-directory, or filesystem-alias guarantee. Unix planning keeps exact path equality, including non-UTF-8 paths, without imposing Windows extension or case folding.

The CLI's native Windows materialization regression runs in the existing Windows CI job. It executes both wrapper formats for a Node shebang control, checks argument and exit propagation, and verifies rejection before any shim write for case, extension, and sigma collisions, preserving sentinel output files. It does not claim exhaustive filesystem casing coverage.

This crate does not execute scripts, enforce a process sandbox, authenticate registries, or itself mutate the filesystem. Runtime platform checks remain required. Other platforms have no supported link strategy in this release.

## Shim collision work fixture

One private accumulator retains entries and generated-output reservations across
registry and workspace population, then sorts once by target and source. Source
validation remains separate: workspace bins inspect the real canonical source
but retain the staged package-link source in the plan. Reservations still precede
interpreter inspection. `validate_targets` shares the same reservation operation.

`shims::tests::large_mixed_fixture_measures_indexed_work` exercises the production
population and insertion paths with 5,000 registry commands (`r0000`–`r4999`) and
5,000 workspace commands (`w0000`–`w4999`) in one output directory. Two real Node
source files serve all mappings. Private per-invocation test counters observe
key generation and set-insertion attempts; no instrumentation is compiled into
production builds. A pairwise baseline compares the same ordered generated
output identities and counts actual prior-entry comparisons.

Measured on Darwin arm64, 2026-10-28, against the working changes based on
`99c2bffa2418cc49551db8ce64fe82c9c479b1ca`:

| Strategy | Successful generated keys | Successful reservation attempts | Pairwise comparisons |
| --- | ---: | ---: | ---: |
| Unix | 10,000 | 10,000 | 49,995,000 |
| Windows | 20,000 | 20,000 | 49,995,000 |

After those successful insertions, a separate workspace package repeats `r0000`.
Both methods reject it. Total generated keys become 10,001 / 20,002 and total
reservation attempts become 10,001 / 20,001 respectively: Windows eagerly
constructs both keys but stops reserving at the first collision. The baseline
finds this duplicate at its first prior entry, totaling 49,995,001 comparisons.
Counters at the registry/workspace transition also verify that registry keys
are generated only once.

Reproduce from the repository root (Node 22 or another supported version):

```sh
CARGO_TARGET_DIR=target node --experimental-strip-types scripts/dev.ts test -p tapid-linker --lib --locked large_mixed_fixture_measures_indexed_work -- --nocapture
```

Reservation attempts are not internal BTree comparisons. These counts establish
elimination of pairwise scanning and registry-key reconstruction, not a
wall-clock or application-level speedup. Broader application benchmarks belong
to #308. Windows strategy tests on Unix do not establish native Windows path
encoding or wrapper execution; the existing native Windows CI regression owns
that evidence.
