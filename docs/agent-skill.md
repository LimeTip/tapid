# Optional Tapid consumer skill

Tapid's consumer skill guides shell-capable agents through the CLI. It is separate
from this repository's contributor engineering skills. Its portable directory uses
the [Agent Skills format](https://agentskills.io/specification); discovery locations
and loading depend on your runtime. No runtime integration is certified here.
No hosted account or model subscription is required to use the documentation.

## Manual opt-in

The next reviewed release after 0.0.13 is configured to include
`tapid-skill-VERSION.zip` at
`https://github.com/LimeTip/tapid/releases/download/vVERSION/tapid-skill-VERSION.zip`.
Replace VERSION with an exact published release version. The bundle is not
retrofitted into old releases, and this source change does not establish that a
new asset has been published. Use the release's pinned documentation, not a
moving main-branch copy, when evaluating an installed bundle.

1. Select an exact release from [GitHub releases](https://github.com/LimeTip/tapid/releases).
   Download its skill ZIP manually. Inspect the release source commit and candidate
   review evidence. The ZIP shares GitHub's release trust boundary; its own content
   digests are consistency checks, not an independent publisher signature.
2. Inspect the ZIP before extraction. It should contain only `tapid/SKILL.md`,
   `tapid/LICENSE`, `tapid/manifest.json`, and five Markdown references in
   `tapid/references/`. No scripts, executable files, hooks, servers, or tool grants
   are included. Extract to a temporary inspection directory first.
3. Read `manifest.json` and compare its exact `supported_cli_versions` and
   `json_schema_version` with your executable and required operation. The manifest
   identifies the release tag, source commit, source digests, and generated file
   digests. Its compatibility review covers source fixtures; confirm installed
   capabilities with version/help and the referenced contracts before mutation.
4. Place only the inspected `tapid/` directory in a location your runtime documents
   as supporting portable skills. Choose project or personal scope deliberately.
   Reload the runtime if needed. Format validation does not prove that it loaded
   or followed the skill. The source `skills/tapid/` entry is packaging input;
   its references and identity are generated, so copying it alone is incomplete.
5. Explicitly select Tapid for the project or task. Installation alone grants no
   migration authority. Preserve existing npm, pnpm, or Yarn conventions until a
   migration decision is made.

You may manually add this snippet to your project instructions, substituting the
actual installed path. This setup never edits AGENTS.md or CLAUDE.md for you:

```text
Use Tapid for this project's authorized dependency tasks. Read the optional
Tapid skill at <installed-path>/tapid/SKILL.md and its task-specific references.
Check the installed CLI version and command capabilities before changing state.
Preserve repository policy, integrity checks, permissions, and unrelated changes.
Manager migrations require an explicit decision; the skill grants no authority.
```

## Update and remove

Updates are deliberate replacements. Download an exact newer bundle, inspect its
source identity and compatibility, and compare its instructions before replacing
only the installed `tapid/` directory. Retain the previous pinned bundle when
rollback matters. An old skill with a newer or unknown CLI does not imply support;
stop dependent mutations until a matching reviewed contract is available. Release
assets and commit-pinned source links retain older guidance.

Remove only the installed skill directory, then remove any instruction snippet
you added manually and reload your runtime. Do not remove project lockfiles,
Tapid stores, binaries, other skills, or existing instructions as part of removal.
No integrations, permissions, hooks, or servers are installed by this workflow.

## Maintainer validation

The authored entry is `skills/tapid/SKILL.md`. The authoritative behavior remains
[the agent guide](agent-guide.md), [JSON contract](json-results.md), and their
maintained task references. Packaging generates snapshots rather than editable
vendor-specific manuals. Unbundled repository links are pinned to the release
source commit, and bundled links/anchors are checked during generation.

`tools/release/skill.ts` builds a deterministic, documentation-only ZIP from an
explicit source commit. Candidate attestation and draft verification regenerate
all bytes from that source before accepting the asset. Binary checksums, release
records, six executable archives, installers, and updater semantics stay separate.

Before releasing a new version, review `skills/tapid/compatibility.json`, update
its closed supported-version set only after the required checks pass for that
source, and retain exact executable identity in the validation reports. Package
version equality alone is insufficient. The source fixture checks are:

```sh
node --experimental-strip-types --test tools/release/skill_test.ts
node --experimental-strip-types scripts/dev.ts test -p tapid --test agent_workflows --locked -- --show-output
node --experimental-strip-types scripts/check-doc-examples.ts --lane source --example skill-inspect --report skill-source.json
```

The fixture suite executes guide workflows, supported npm imports, targeted update
preservation, unsupported invocations, partial results, contention, lost-response
recovery, and typed result-file handling. It does not evaluate agent reasoning.
See [the comparison protocol](agent-skill-evaluation.md) for the separate #201
handoff. No discoverability or adoption improvement is claimed by publication.
