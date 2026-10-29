# Review #248 implementation evidence

This is an incomplete implementation checkpoint, not acceptance or closure of #248. Original criteria remain authoritative; pending work below is not waived. Source base: `99c2bffa2418cc49551db8ce64fe82c9c479b1ca`; the engine records the resulting source snapshot. Changes are uncommitted in the managed worktree; no release, publication, merge, issue update or deployment was executed.

## Validation environment and receipts

Date: 2026-10-28. macOS arm64. Node `/Users/doug/.relay/toolchains/node-v22.23.3-darwin-arm64/bin/node` (22.23.3), with its directory explicitly prefixed to PATH; Rust commands use `scripts/dev.ts` and `CARGO_TARGET_DIR=target`. PowerShell 7.6.9 was obtained from the official PowerShell GitHub release into temporary storage and prefixed to PATH for installer tests. This is not native Windows evidence.

Commands actually run (all Node commands use the executable/PATH above):

- `node --experimental-strip-types scripts/dev.ts test -p tapid-archive --lib --locked extraction_`: 9 passed before changes.
- `node --experimental-strip-types scripts/dev.ts test -p tapid-archive --all-features --locked`: final 33 passed.
- `node --experimental-strip-types scripts/dev.ts clippy -p tapid-archive --all-targets --all-features --locked -- -D warnings`: passed.
- `node --experimental-strip-types scripts/dev.ts fmt --all --check`: passed after final Rust test additions.
- `node --experimental-strip-types --test tools/check_architecture_test.ts`: baseline 12 passed; failing renamed-CLI dependency regression reproduced before implementation; extended 38 passed.
- `node --experimental-strip-types tools/check_architecture.ts`: passed against current workspace, 187 production Rust files scanned, advisory sizes preserved.
- `node --experimental-strip-types --test tools/release/installer_download_test.ts`: initial regression failed on synchronous body read after establishing the handler seam; implementation passed all nine cases against each of two installer forms.
- `node --experimental-strip-types --test tools/release/candidate_test.ts`: 16 baseline tests passed before private consolidation; final 32 passed.
- `node --experimental-strip-types scripts/dev.ts test -p tapid-registry-client --all-features --locked`: baseline 82 passed; final 84 passed.
- `node --experimental-strip-types scripts/dev.ts test -p tapid-registry-client --lib --locked fetch_applies_shared_json_response_validation`: 2 passed (not a zero-test filter).
- `node --experimental-strip-types scripts/dev.ts clippy -p tapid-registry-client --all-targets --all-features --locked -- -D warnings`: passed.
- `node --experimental-strip-types scripts/dev.ts test -p tapid-store --all-features --locked`: baseline 59 passed; final 60 passed.
- `node --experimental-strip-types scripts/dev.ts clippy -p tapid-store --all-targets --all-features --locked -- -D warnings`: passed.
- `node --experimental-strip-types scripts/dev.ts test -p tapid-lockfile --all-features --locked`: baseline 66 passed; final 67 passed.
- `node --experimental-strip-types scripts/dev.ts clippy -p tapid-lockfile --all-targets --all-features --locked -- -D warnings`: passed.
- `node --experimental-strip-types --test tools/release/*_test.ts`: final complete run passed, 356 passed / 4 skipped / 0 failed (360 tests). The skips are two native Windows probes and two native Fish startup probes because Fish is unavailable. The PowerShell download cases executed, rather than skipping.
- `node --experimental-strip-types scripts/dev.ts test -p tapid --lib --locked invalid_archive_is_typed_without_activating_any_packages`: 1 passed.
- `node --experimental-strip-types scripts/dev.ts test -p tapid --test cli --locked install_preserves_project_and_store_on_integrity_and_archive_failures`: 1 passed, exercising integrity, malformed gzip and oversized-header failure cases.

An earlier broad run was interrupted after output paused at the generated PowerShell PATH fixtures, and a 30-second diagnostic timed out. The focused fixture subsequently passed in 91.5 seconds, and the complete suite passed in 231.9 seconds. Those successful reruns supersede the timeout; no unresolved test failure or sandbox restriction is asserted. Diagnostic tracing and temporary test edits were removed. No audit policy was suppressed.

Full integrated local lane and independent QA/Acceptance remain pending, as do existing hosted native-platform, compatibility, security, coverage, packaging and MSRV gates after publication. No hosted evidence is a prerequisite to continuing the remaining coding slices.

## Child ledger

### [#232](https://github.com/LimeTip/tapid/issues/232) — fix(lifecycle): preserve overlapping dependency sections during update --latest

Original acceptance criteria:

- [ ] Regressions cover development/peer and regular/optional overlaps, both explicit names and update-all.
- [ ] Updating preserves dependency sections and peer declarations, with deterministic handling of repeated names.
- [ ] Add retains its documented section-move behavior.
- [ ] Failed resolution or activation preserves manifests, lockfile, store state, and active installation.

Implementation / evidence / remaining work:

Retained; fresh acceptance audit pending. Inspect `crates/tapid-cli/src/application/lifecycle.rs` and lifecycle integration tests for all overlap/failure criteria. No implementation was repeated and no current lifecycle receipt is claimed.

### [#233](https://github.com/LimeTip/tapid/issues/233) — fix(installer): preserve shell profiles without a trailing newline

Original acceptance criteria:

- [ ] Test nonempty profiles with and without final LF, empty profiles, and an existing managed block.
- [ ] Preserve the prior shell command and ensure both markers occupy standalone lines.
- [ ] Repeated installation is idempotent; uninstall removes only managed content.
- [ ] Failed validation leaves the original file intact.

Implementation / evidence / remaining work:

Retained without repeating production changes. The complete release suite passes the Unix PATH fixtures for nonempty profiles with and without final newline, empty profiles, existing owned blocks, repeated installation/uninstall and failed validation. See `tools/release/installers_test.ts`: `Unix PATH management preserves ... across repeated install and uninstall`, `Unix installer owns an idempotent POSIX PATH block and uninstall removes only that block`, and validation/replacement preservation tests. Native shell execution is local macOS evidence; existing hosted platform gates remain.

### [#234](https://github.com/LimeTip/tapid/issues/234) — fix(archive): enforce resource limits during archive traversal

Original acceptance criteria:

- [ ] Instrumented tests prove excessive input is rejected before later entries or payloads are traversed.
- [ ] Cover excessive entry count, oversized entries, aggregate size, and compressed input.
- [ ] Preserve existing traversal, special-file, collision, and reserved-path rejection.
- [ ] Failure removes incomplete staging output and does not activate or publish package content.

Implementation / evidence / remaining work:

Existing bounded traversal was reused; no replacement parser or production behavior was added. `crates/tapid-archive/src/lib.rs` validates logical headers with `EntryValidator` and bounds parser reads with `ParserBudgetReader`. Existing observable tests prove count/aggregate/path limits precede invalid later headers, oversized entries precede missing payload, PAX effective sizes govern limits, and GNU/PAX metadata is bounded for tar and gzip. Added `compressed_input_limit_precedes_parsing_and_staging_creation`: invalid tar/gzip framing with a 17-byte input and 16-byte limit returns `ArchiveTooLarge` before creating staging. Added `reserved_archive_metadata_rejection_cleans_preceding_extracted_files` for tar/gzip cleanup after a preceding ordinary file. All 33 archive tests, archive Clippy and formatting pass, retaining traversal/type/collision rejection. The CLI library no-activation regression and integration rollback regression pass; the latter exercises integrity, malformed-gzip and oversized-header failures with no prior manifest/lockfile/layout/store changes or staging residue. These are deterministic local fixtures, not exploit validation or a total-process resource benchmark. Cleanup remains an attempt under filesystem failure, as documented.

### [#235](https://github.com/LimeTip/tapid/issues/235) — perf(archive): stream file contents when computing canonical tree digests

Original acceptance criteria:

- [ ] Existing canonical digests remain unchanged for all supported fixtures.
- [ ] File-content buffering is bounded independently of total tree size.
- [ ] Tests cover executable/non-executable files, empty files, ordering, reserved metadata, and unsafe file types.
- [ ] A representative large-tree benchmark or instrumentation demonstrates bounded buffering.

Implementation / evidence / remaining work:

Existing streaming preserved in `crates/tapid-archive/src/lib.rs`. The 33-test archive receipt includes canonical executable/order/type behavior and `digest_reader_requests_at_most_one_fixed_chunk`. Remaining: representative whole-tree instrumentation and criterion-by-criterion digest reconciliation. A reader slice bound is not a total-memory measurement.

### [#236](https://github.com/LimeTip/tapid/issues/236) — fix(store): verify competing artifact destinations before returning AlreadyPresent

Original acceptance criteria:

- [ ] A deterministic input-reader hook creates a competing destination during ingestion.
- [ ] Reject wrong bytes, symlinks, and non-regular destinations on the race branch.
- [ ] Accept a matching regular artifact from another writer.
- [ ] Preserve the competing destination and clean up private staging files on every outcome.

Implementation / evidence / remaining work:

Delivered verifier retained in `crates/tapid-store/src/lib.rs`; both lookup and competing-publication branches call `verify_existing_artifact` on a no-follow regular-file handle. Baseline 59 store tests passed. Reused one first-read hook across existing wrong/matching/symlink reader fixtures and added a competing-directory case, avoiding another copied reader. All four race tests assert destination preservation and empty staging; the directory test also retains a competitor-owned child. Final 60 store tests and all-target/all-feature Clippy pass. Native Windows symlink handling remains subject to its existing privilege-aware hosted probe; no native Windows receipt is claimed locally.

### [#237](https://github.com/LimeTip/tapid/issues/237) — fix(installer): bound Windows download redirects and response-body reads

Original acceptance criteria:

- [ ] A deterministic stalled-stream/server test terminates within the configured deadline.
- [ ] Redirect chains cannot reset or evade the total deadline.
- [ ] Timeout releases response/stream resources and removes partial downloads.
- [ ] Successful downloads and size-limit rejection retain existing behavior.

Implementation / evidence / remaining work:

Implemented in `scripts/install.ps1`: a single 120-second cancellation source covers manual redirects, asynchronous body reads and writes. Existing HTTPS, ten-redirect cap, byte limits, disposal and original-error precedence remain. A private injected handler and timeout enable offline fixtures. `tools/release/installer_download_test.ts` runs nine cases against both the source template and generated installer: stalled headers, partial stalled body, redirects consuming the same deadline, success, advertised/actual oversize, redirect cap, downgrade, and missing location. The regression failed on synchronous `Read` before the deadline implementation and passes afterward. Modern PowerShell 7.6.9 on macOS was exercised; native Windows and supported older runtime behavior remain hosted obligations. The complete release suite passes; native Windows remains a later platform gate. The four-argument Stream.ReadAsync cancellation overload is available from .NET Framework 4.5; see [Microsoft Stream.ReadAsync documentation](https://learn.microsoft.com/en-us/dotnet/api/system.io.stream.readasync?view=netframework-4.8). [Microsoft HttpCompletionOption documentation](https://learn.microsoft.com/en-us/dotnet/api/system.net.http.httpcompletionoption) explains why ResponseHeadersRead needs a separate body deadline.

### [#238](https://github.com/LimeTip/tapid/issues/238) — refactor(architecture): move shared dependency interpretation out of CLI orchestration

Original acceptance criteria:

- [ ] Identify current rules and owners before moving implementation.
- [ ] Preserve alias names, exact registry identity, optional declarations, workspace fallback rejection, overrides, and peer semantics.
- [ ] Online and offline/frozen paths use the shared interpreted declarations.
- [ ] Existing behavior tests pass before and after the private refactor.
- [ ] Update architecture/contracts and add an ADR only where the decision changes shared ownership or contracts.

Implementation / evidence / remaining work:

Not implemented in this checkpoint. Inventory remaining rules in `crates/tapid-cli/src/online/resolution.rs`, their replay consumers, and manifest/resolver interfaces before moving one cohesive pure rule. Keep registry routing and transactions in CLI; record shared ownership decisions in an ADR where consequential. All original criteria remain open.

### [#239](https://github.com/LimeTip/tapid/issues/239) — refactor(lockfile): expose typed contexts and remove duplicate CLI parsing

Original acceptance criteria:

- [ ] Replay no longer implements a second percent/context parser.
- [ ] Typed accessors preserve scoped peers, percent encoding, empty contexts, and supported platform values.
- [ ] Test canonical round trips and rejection of duplicate, reordered, malformed, and noncanonical fields.
- [ ] Preserve supported legacy schema behavior and exact instance identity.
- [ ] Keep implementation modules private with deliberate public re-exports.

Implementation / evidence / remaining work:

Delivered typed contexts retained in private `crates/tapid-lockfile/src/model.rs` with deliberate root exports. Replay calls `key.peer_context()` and `key.platform_context()` rather than decoding context strings itself. Baseline 66 lockfile tests passed. Added an independent literal scoped-peer/percent-encoded key round trip, empty peer/platform contexts and a Windows/aarch64 platform with empty libc. Extended mutation rejection with reordered peers, lowercase percent escapes and malformed triplets. Existing duplicate/noncanonical/platform-order and legacy schema 4/6 tests remain. Final 67 lockfile tests and all-target/all-feature Clippy pass; no production parser or public interface changed.

### [#240](https://github.com/LimeTip/tapid/issues/240) — test(architecture): enforce crate dependency direction and test-support isolation

Original acceptance criteria:

- [ ] Fixture tests reject forbidden normal/build dependencies and accept legitimate dev dependencies.
- [ ] Check target-specific and renamed dependencies where applicable.
- [ ] Diagnostics identify the offending crate, dependency, and violated rule.
- [ ] Run the checks in the existing architecture lane.
- [ ] Preserve advisory file-size behavior and the separate hard entrypoint limit.

Implementation / evidence / remaining work:

Implemented in `tools/check_architecture.ts` and `tools/check_architecture_test.ts`. The existing checker invokes locked/offline `metadata --no-deps --format-version 1` through `scripts/dev.ts`, inspects actual package identity (not the rename), kind and target predicate, forbids production/build dependencies on CLI and test support, and restricts core to base64/semver/url. Dev dependencies remain allowed. Diagnostics name both crates, rename, dependency kind, target and rule. Acquisition failures fail closed. Baseline 12 tests passed; extended 38 tests passed, including normal/build/dev and renamed/target-specific matrices plus metadata failure. The actual workspace check passed with 187 tracked production Rust files and retained size advisories/entrypoint enforcement. `docs/architecture.md` documents the direct-dependency scope; transitive purity is not proven.

### [#241](https://github.com/LimeTip/tapid/issues/241) — perf(install): index materialization sources and verify each distinct tree once per phase

Original acceptance criteria:

- [ ] Shared-source fixtures prove lookup and digest work follows distinct sources rather than placements.
- [ ] Reject inconsistent digest mappings before copying.
- [ ] Detect source changes during materialization without skipping the final verification phase.
- [ ] Preserve replay snapshot cleanup, rollback, and progress completion.
- [ ] Report a representative benchmark or counter comparison.

Implementation / evidence / remaining work:

Existing source index retained in `crates/tapid-cli/src/filesystem/tree.rs`. Remaining: distinct-source phase counters, inconsistent mappings, source mutation, final verification, replay recovery/cleanup/progress receipts and representative comparison. No performance result is claimed from source inspection.

### [#242](https://github.com/LimeTip/tapid/issues/242) — perf(linker): share shim insertion and index generated output collisions

Original acceptance criteria:

- [ ] Preserve ASCII case, extension, sigma, and tested Unicode collision rejection on Windows.
- [ ] Preserve exact Unix paths, including non-UTF-8 names.
- [ ] Detect collisions between registry and workspace shims.
- [ ] Preserve deterministic plan ordering and rejection before materialization writes.
- [ ] Benchmark or instrument a large fixture to establish improvement.

Implementation / evidence / remaining work:

Existing generated-output reservation retained in `crates/tapid-linker/src/shims.rs`. Remaining: current Unix/non-UTF-8 and Windows case/extension/sigma/Unicode/cross-source collision evidence, deterministic ordering/prewrite rejection, representative counters or benchmark. No native Windows receipt is claimed.

### [#243](https://github.com/LimeTip/tapid/issues/243) — perf(lifecycle): cache outdated metadata by registry and actual package name

Original acceptance criteria:

- [ ] A counting transport verifies one metadata retrieval for two aliases targeting the same package.
- [ ] Overlapping sections reuse metadata while producing separate declaration results.
- [ ] Distinct registry origins remain isolated, including routing and credentials.
- [ ] Fixture parsing happens once and retains current error handling.
- [ ] Failures and result ordering are deterministic.

Implementation / evidence / remaining work:

Existing command metadata cache retained in CLI lifecycle code. Remaining: transport retrieval counters for same-origin actual package aliases and overlapping declarations, origin/credential isolation, fixture-once parsing, deterministic failures and order. No request reduction measurement is claimed.

### [#244](https://github.com/LimeTip/tapid/issues/244) — refactor(registry): share npm and JSR JSON response validation helpers

Original acceptance criteria:

- [ ] Both adapters use the shared response/object checks.
- [ ] Cover rejected status, missing/invalid content type, accepted JSON suffixes, malformed JSON, and non-object roots.
- [ ] Existing npm/JSR metadata tests pass before and after the refactor.
- [ ] No new public module, generic utility crate, or transport abstraction is introduced solely for these helpers.

Implementation / evidence / remaining work:

Present private `response::{json_response,json_object}` remains shared by npm and JSR; no production helper or transport was added. Baseline registry tests: 82 passed. Added fetch-level adapter regressions using existing fakes for missing/invalid content type, rejected statuses, malformed JSON, non-object roots, and case-insensitive +JSON suffix acceptance. npm's separate 404-as-no-candidates contract remains covered by its existing test; JSR rejects 404. Two new focused tests pass; the final affected-crate suite passes 84 tests and all-target/all-feature Clippy passes. Both adapters continue to use the private shared validators.

### [#245](https://github.com/LimeTip/tapid/issues/245) — refactor(release): share signed and unsigned candidate asset verification

Original acceptance criteria:

- [ ] Both attestation paths use shared checks for local byte/layout consistency.
- [ ] Mutation tests/fixtures reject changed archives, checksums, regenerated records, and installers in both paths.
- [ ] Signed-only evidence and unsigned-only schema rules remain explicit.
- [ ] Temporary resources are cleaned on success and failure.
- [ ] Existing release-tool tests pass before and after the refactor.

Implementation / evidence / remaining work:

Extracted private `verifyCandidateContents` in `tools/release/candidate.ts` for checksum/record consistency, archive copying, installer regeneration/comparison and finally cleanup. Both callers retain their existing `fileDigest` hashing and limits. Layout rules remain separate because unsigned attestation tolerates the detached signature sidecar whereas signed attestation requires exactly eleven assets and binds provider IDs/sizes. Signed/unsigned schemas and authorization were preserved. Baseline 16 candidate tests passed before refactoring; final 32 pass, including fourteen same-size mutation/missing/extra regressions and two process-isolated cleanup tests covering success and regeneration failure for both paths. Cleanup fixtures set private TMPDIR/TEMP/TMP and verify no regenerated resources remain. The complete release suite passes (356 passed, four platform/tool skips).

### [#246](https://github.com/LimeTip/tapid/issues/246) — ci: verify the declared Rust 1.88 minimum with a locked build

Original acceptance criteria:

- [ ] CI exercises Rust 1.88 with checked-in lockfiles.
- [ ] The job verifies the supported production packages and documented feature set.
- [ ] A fixture or intentional compiler-incompatible change demonstrates that the lane fails.
- [ ] Normal stable/native platform validation remains intact.
- [ ] Any required minimum-version change is deliberate and reflected in package metadata and documentation.

Implementation / evidence / remaining work:

Locked Rust 1.88 workflow retained. `.github/workflows/ci.yml` pins 1.88.0 and runs `cargo check --workspace --all-targets --all-features --locked` without failure suppression; stable/native tests remain separate. The complete release suite passes `CI verifies the declared Rust 1.88 minimum on the locked production workspace` and the intentional compiler-failure fixture control. No local Rust 1.88 build was run; the exact-head hosted MSRV job remains a later gate, not an inferred success from workflow text.

### [#247](https://github.com/LimeTip/tapid/issues/247) — refactor(release): centralize target definitions and stable-version comparison

Original acceptance criteria:

- [ ] Build, packaging, candidate verification, and installer target expectations agree.
- [ ] Unknown/duplicate targets and missing/extra expected assets fail clearly.
- [ ] Shared version tests cover ordering, invalid syntax, and caller-specific rejection rules.
- [ ] Independent expected-value fixtures remain able to detect a mistake in the shared source.
- [ ] Adding a supported platform has one production definition plus deliberate platform tests.

Implementation / evidence / remaining work:

Not implemented in this checkpoint. Existing `tools/release/release.ts` TARGETS and `versions.ts` remain. Enrich the production target contract and validate static workflows/installers against independent fixtures; preserve caller-specific version rules. No credit is assigned to unmerged PR #288.

## Executable continuation

Inventory and move one shared declaration interpretation rule for #238, beginning with `manifest_roots` in `crates/tapid-cli/src/online/resolution.rs` and its online/replay consumers. Capture their existing behavior first, retain CLI routing/transactions, and validate both consumers after extraction to the owning capability. Continue #235/#241–#243 measurements, #247 target-contract consolidation and #232 delivered-fix reconciliation. Run the integrated lane after final coding and update every ledger entry with actual receipts. Retain explicit native Windows obligations; do not mark the umbrella accepted from these focused results.

