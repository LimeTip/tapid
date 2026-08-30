# ADR 0003: Treat production file length as advisory

Status: Accepted
Date: 2026-08-30
Supersedes: The source-length enforcement decision in ADR 0002

## Context

ADR 0002 correctly rejected 800 lines as an absolute Rust language rule, but its enforcement still required every larger production file to be split or listed as an exception. That made a general architecture recommendation behave as a hard CI limit.

Physical line count can identify code worth reviewing, but it cannot determine whether a module is cohesive, deep, navigable, or correctly placed. A forced split can make architecture worse by creating shallow modules, leaking implementation details, or scattering one responsibility across files.

The CLI entrypoint is different. Its responsibility is intentionally narrow and objectively testable, so its 100-line limit remains a hard contract.

## Decision

Eight hundred physical lines remains a general review recommendation for tracked production Rust files. Crossing it causes `scripts/check_architecture.py` to report an advisory while returning success when no hard rule is violated.

Reviewers evaluate cohesion, interface depth, change locality, navigability, and whether a real seam exists. A file is split only when the change creates clearer responsibility or a deeper module. A cohesive file may remain larger than 800 lines without an exception.

The 100 physical line limit for `crates/tapid-cli/src/main.rs` remains enforced. `docs/architecture-exceptions.txt` is reserved for documented temporary exceptions to explicit hard architecture limits, not advisory recommendations.

## Alternatives considered

### Remove the source-length recommendation

Rejected. A consistent review trigger still helps identify files where responsibility, locality, and navigability deserve attention.

### Raise the recommendation

Not selected because there is no repository evidence supporting a better numeric trigger. Eight hundred remains a useful prompt for review, and it can be changed later based on observed maintenance outcomes.

### Keep mandatory exceptions above 800 lines

Rejected because mandatory exceptions turn a recommendation into a hard gate and encourage compliance paperwork instead of architectural judgment.

## Consequences

- CI reports production files above 800 lines without failing solely because of their length.
- Reviewers must explain architectural concerns in terms of cohesion, depth, locality, navigability, or seam placement rather than line count alone.
- Large cohesive modules do not require exception entries.
- The thin CLI entrypoint remains protected by a hard, documented threshold.
- Exception validation applies only to explicit hard architecture limits.

## Verification

Run:

```text
python3 -m unittest tests.test_check_architecture -v
python3 scripts/check_architecture.py
cargo test -p tapid --test architecture --locked
```

The checker must return success for an ordinary tracked production Rust file above 800 lines while printing a deterministic advisory. It must still fail when `crates/tapid-cli/src/main.rs` exceeds 100 lines without a valid temporary exception.
