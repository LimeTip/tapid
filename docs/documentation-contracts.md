# Executable documentation contracts

Owner: **LimeTip/tapid maintainers**. The CLI repository owns the runnable
examples; the website imports them from a reviewed, pinned public checkout.
No website files, release publication, rulesets or deployment are changed here.

## Website interface (schema version 1)

`docs/examples/contracts.json` inventories `examples` and bounded `capabilities`.
Each example has a stable `id`, a root-relative `file`, explicit `platforms`,
`lanes`, `network`, `prerequisites`, `assertions` and documentation `surfaces`.
The shared shell IDs are `quickstart`, `init`, `upgrade` and `upgrade-help`.
Render the complete UTF-8 LF file, trimming only its final newline; reject
unknown IDs and do not maintain another editable command copy.

`quickstart.sh` follows the current public sequence: create and enter `demo`,
initialize, install `is-char`, then replay offline and frozen. `init.sh` documents
initializing the already-existing current directory. The CLI's existing-directory
and non-overwrite contracts have not changed.

`quickstart.ps1` is a separate native PowerShell example tested by
`scripts/check-doc-examples.ps1`. It is intentionally outside the shared shell
inventory until the website supports native platform rendering. It is not a
translation performed by the Unix runner.

Capability `verified_releases` lists versions actually checked. Self-upgrade's
`first_supported_release: "v0.0.10"` and `expected_releases: ["v0.0.10", "v0.0.11"]` are
reviewed implementation expectations, **not published execution evidence**;
its `verified_releases` remains empty until actual published verification is
reviewed. v0.0.9 lacks `upgrade` and retains reinstall guidance. `upgrade-help`
expects exit 2 plus the unrecognized-subcommand message for v0.0.9, and exit 0
for v0.0.10 and v0.0.11. The published `upgrade` journey explicitly skips v0.0.9 as unsupported
without executing it; the negative help probe still runs separately. Only the
exact v0.0.10 and v0.0.11 source tags have reviewed expectations for the journey;
unknown tags, including newer versions, fail closed pending review.
Do not interpret `first_supported_release` as an open-ended semver allowlist.

A staged local checkout is development input, not a fabricated published pin.
A local commit can unblock clean pinned builds without authorizing a push.

## Running the Unix lanes

Requires Python 3.11+ (CI selects 3.12), Rust for source builds, and macOS/Linux.

```sh
python3 -m unittest discover -s tests -p test_doc_examples.py -v
python3 scripts/check-doc-examples.py --lane source --example init --example upgrade-help --report source.json
```

The source lane builds into a new dedicated temporary target directory and uses
Cargo's executable artifact message, not `target/debug/tapid`. It compares the
binary's version with **the CLI package** version (not the independently versioned
workspace default). It records HEAD and dirty status; a dirty local run is not
proof of a committed tree. Source builds may download Cargo dependencies, but the
selected example commands are offline.

The published lane requires `--binary`, `--expected-sha256`, `--expected-version`
(the exact `tapid X.Y.Z` output), `--release-tag`, named examples and `--report`.
Network examples additionally require `--allow-network`. The existing public
installer workflow supplies the actual installed artifact and retains the digest
of the public installer script. It never builds a replacement if installation
fails. `--release-source-sha` records the separately resolved release tag target;
`source_sha` identifies the runner/docs checkout, not the release binary's source.

The Unix `upgrade` journey in both lanes additionally requires
`--upgrade-target-sha256` and `--upgrade-target-version` identifying the expected
published destination executable. Only the explicit upgrade command may change
the digest. The runner checks the destination digest **before executing** its
version probe and records persisted signature/checksum verification state.
The disposable copy receives its own managed marker; this does not claim an
unmanaged user installation is upgradeable. The published smoke uses the selected
installed release as its source, and independently installs current latest through
discovery to obtain the exact destination executable digest and version. A latest
release changing during the run fails these checks rather than relaxing them.
The upgrade step has a five-minute ceiling and retains its JSON report with
`always()` after execution, including failure or unsupported-skip reports.

When selected and latest tags match, the canonical journey checks a same-release
upgrade. The public smoke also installs the highest earlier supported stable
release from a bounded list of 100 releases and upgrades it to latest. Releases
before v0.0.10 cannot perform that transition. For latest versions newer than
v0.0.10, the smoke then repeats the upgrade and requires an "already up to date"
message and unchanged executable bytes. Historical versions through v0.0.10 skip
this output assertion because they always reported reinstallation as an upgrade.

These public installation and upgrade checks run on Linux daily, and on Linux,
macOS and Windows for release/manual runs. Both tagged installers and the actual
`https://tapid.dev/install.sh` and `.ps1` paths are tested. Before execution, each
public script must match its separately downloaded latest-tag script exactly,
even when testing an older selected release. Public installers must also produce
the same selected-version binary as the tagged installers; latest
discovery also runs through the public scripts. The canonical documentation
upgrade runner remains Unix-only. Windows has separate native workflow upgrade
checks and its native quickstart.

These configuration changes do not prove published execution of v0.0.10 or
v0.0.11. Self-upgrade's verified-release list remains empty.

## Execution and evidence boundaries

The Unix runner deliberately accepts a tiny single-line shell vocabulary:
`mkdir NAME`, `cd NAME`, the maintained init/install/version commands and upgrade
commands. It validates the entire example before execution and rejects shell
operators, substitutions, assignments, extra `cd` arguments, absolute paths and
parent traversal. Commands execute sequentially; `cd` changes the retained
working directory. No prerequisites or flags are inserted. Unsupported shell
syntax fails rather than being silently reinterpreted. Native PowerShell uses its
own similarly constrained vocabulary and native commands.

Each journey gets a disposable project, HOME and binary copy. Child environments
are allowlisted, excluding registry credentials and inherited TAPID overrides.
Unix commands have a 120-second timeout and 64-KiB combined-output ceiling;
version probes have a 10-second timeout. Source builds have a ten-minute limit
and eight-MiB output ceiling. Inputs and lockfile snapshots are bounded. Native
PowerShell has a 120-second/65,536-character command ceiling. Process-group/tree
cleanup is best effort: neither runner is an OS sandbox. Do not run unreviewed
code with secrets or privileged tokens.

JSON reports use `schema_version: 1`, explicit lane, platform, release identity,
binary path/digest/version, literal command exit codes, bounded output and
assertions. A reviewed unsupported release produces `status: "skipped"`, an
explicit reason and no commands; it exits zero but is never reported as passed
upgrade evidence. Any failed selected example still fails the overall report.
Unix reports group results under `examples`; native reports contain
one journey's `commands` directly. Reports also bind script bytes by SHA-256.
`failure_class` distinguishes contract, provenance, command, assertion, timeout,
output-limit and execution failures. A command/network failure is **not** silently
retried into success or automatically classified as a product/security defect;
inspect evidence and classify upstream unavailability separately. Reports are
ordinary CI evidence, not cryptographic publisher attestations.

## CI integration and remaining gates

Existing required Unix `Test (...)` jobs run unit tests and source init/upgrade
capability contracts without making PRs depend on npm. The Unix jobs also run
`scripts/check-release-record.py --binary target/debug/tapid` after building the
CLI. That offline check sends the real generator's six-platform release record
through the Unix installer and compiled updater, checks repeat upgrades, changes
the artifact host while keeping discovery fixed, and rejects malformed metadata
even with a valid recovery cache. It uses temporary shell fixture executables and
an exact local URL mapping for curl, with no public network requests. This checks
agreement between the implementations; it is not published binary evidence.
Existing Rust consumer and upgrade tests cover deterministic rollback/tamper checks.
The public installer smoke workflow retains release events, adds a manual stable
tag selector, and declares daily **Linux-only** checks. Release/manual runs retain
macOS and Windows. The resolver captures latest tag, selected release target and
trusted main runner SHA once; all jobs check out that exact runner SHA. Historical
selected-version installation and current latest discovery have separate expected
versions. Third-party actions in the public installer smoke workflow are
commit-pinned, its permissions stay `contents: read`, and its JSON/script-digest
artifacts have 30-day retention. Merging activates the
schedule; this local change has not run hosted CI or changed repository settings.

The public lane tests real package installation before reusing the existing empty
consumer fixture for root-script forwarding, exit 41 and lifecycle suppression.
That supplemental fixture is not package-installation evidence. PowerShell
fixture execution on macOS does not prove Windows OS compatibility; native Windows
CI remains an external verification gate.

Automatic issue deduplication/recovery, required-check configuration, website pin
promotion, release/deployment and post-deployment read-back remain separately
owned/gated work. No automatic promotion is implemented here. Failures remain
visible in Actions; integrity failures require human review, not fallback to a
cached or locally built binary. Review Actions allowances before enabling schedules
in any private fork; no paid CI expansion is authorized.
