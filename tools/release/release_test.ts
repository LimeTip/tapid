import {
  match as assertMatch,
  ok as assert,
  rejects as assertRejects,
  strictEqual as assertEquals,
  throws as assertThrows,
} from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, truncate, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { arch, platform } from "node:process";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { promisify } from "node:util";
import { checksumLines, releaseRecord, releaseVersion } from "./release.ts";

const root = fileURLToPath(new URL("../../", import.meta.url));
const text = (path: string) => readFile(join(root, path), "utf8");
const execFileAsync = promisify(execFile);

test("release tag must be stable semver and match tapid", () => {
  assertEquals(releaseVersion("v1.2.3", "1.2.3"), "1.2.3");
  for (const tag of ["1.2.3", "v1.2", "v1.2.3-rc.1", "v01.2.3", "v1.02.3", "v1.2.03", "main"]) {
    assertThrows(() => releaseVersion(tag, "1.2.3"));
  }
  assertThrows(() => releaseVersion("v1.2.3", "1.2.4"));
  assertThrows(() => releaseVersion("v18446744073709551616.2.3", "18446744073709551616.2.3"));
});

test("release metadata binds all six archives to actual bytes and provider-neutral URLs", async () => {
  const directory = await mkdtemp(join(tmpdir(), "tapid-release-record-"));
  const targets = [
    "aarch64-apple-darwin", "aarch64-pc-windows-msvc", "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu",
  ];
  try {
    const version = "2.3.4";
    for (const target of targets) {
      await writeFile(join(directory, `tapid-${version}-${target}.tar.gz`), Buffer.from(`archive\0${target}\n`));
    }
    const checksums = await checksumLines(directory, version);
    for (const base of [
      "https://github.com/LimeTip/tapid/releases/download/v2.3.4",
      "https://downloads.example.test/tapid/2.3.4/",
    ]) {
      const record = await releaseRecord(directory, version, base);
      const rows = record.split("\n");
      assertEquals(rows.shift(), "tapid-release-v1\t2.3.4");
      assertEquals(rows.pop(), "", "record ends with a newline");
      assertEquals(rows.length, 6);
      for (const [index, row] of rows.entries()) {
        const [target, name, size, digest, url, extra] = row.split("\t");
        assertEquals(extra, undefined);
        assertEquals(target, targets[index]);
        assertEquals(name, `tapid-${version}-${target}.tar.gz`);
        const bytes = await readFile(join(directory, name));
        assertEquals(size, `${bytes.length}`);
        assertEquals(digest, createHash("sha256").update(bytes).digest("hex"));
        assert(checksums.includes(`${digest}  ${name}\n`));
        assertEquals(url, `${base.replace(/\/+$/, "")}/${name}`);
      }
    }
    await writeFile(join(directory, `tapid-${version}-${targets[0]}.tar.gz`), "");
    await assertRejects(() => releaseRecord(directory, version, "https://example.test/2.3.4"), /archive size/);
    await truncate(join(directory, `tapid-${version}-${targets[0]}.tar.gz`), 512 * 1024 * 1024 + 1);
    await assertRejects(() => releaseRecord(directory, version, "https://example.test/2.3.4"), /archive size/);
    await rm(join(directory, `tapid-${version}-${targets[0]}.tar.gz`));
    await assertRejects(() => releaseRecord(directory, version, "https://example.test/2.3.4"), /exactly these archives/);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("release metadata rejects unsafe or ambiguous URL directories before reading archives", async () => {
  for (const base of [
    "http://example.test/v1.2.3", "https://", "https://user:password@example.test/v1.2.3",
    "https://user@example.test/v1.2.3", "https://example.test/v1.2.3#fragment",
    "https://example.test/v1.2.3?query", "https://example.test/v1.2.3\\extra",
    "https://example.test/v1.2.3\tmore", "https://example.test/v1.2.3\nmore",
    "https://example.test/v1.2.3 more", "https://example.test/v1.2.3\0more",
    "https://example.test/caf\u00e9",
    "https://[::1]/v1.2.3", "https://example.test:/v1.2.3",
  ]) await assertRejects(() => releaseRecord("/missing-directory", "1.2.3", base), /URL/);
  await assertRejects(() => releaseRecord("/missing-directory", "01.2.3", "https://example.test/v01.2.3"), /stable semver|vX.Y.Z/);
});

test("metadata CLI writes the record beside checksums without replacing existing release files", async () => {
  const directory = await mkdtemp(join(tmpdir(), "tapid-release-cli-"));
  try {
    for (const target of [
      "aarch64-apple-darwin", "aarch64-pc-windows-msvc", "aarch64-unknown-linux-gnu",
      "x86_64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu",
    ]) await writeFile(join(directory, `tapid-1.2.3-${target}.tar.gz`), target);
    const checksums = await checksumLines(directory, "1.2.3");
    await writeFile(join(directory, "SHA256SUMS"), checksums);
    const base = "https://storage.example.test/tapid/1.2.3";
    await execFileAsync(process.execPath, ["--experimental-strip-types", join(root, "tools/release/release.ts"), "metadata", directory, "1.2.3", base]);
    assertEquals(await readFile(join(directory, "tapid-release-v1.tsv"), "utf8"), await releaseRecord(directory, "1.2.3", base));
    assertEquals(await readFile(join(directory, "SHA256SUMS"), "utf8"), checksums);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("checksum output requires exactly six release archives", async () => {
  const directory = await mkdtemp(join(tmpdir(), "tapid-release-"));
  try {
    const targets = [
      "aarch64-apple-darwin",
      "aarch64-pc-windows-msvc",
      "aarch64-unknown-linux-gnu",
      "x86_64-apple-darwin",
      "x86_64-pc-windows-msvc",
      "x86_64-unknown-linux-gnu",
    ];
    for (const target of targets) {
      await writeFile(join(directory, `tapid-1.2.3-${target}.tar.gz`), target);
    }
    const output = await checksumLines(directory, "1.2.3");
    assertEquals(output.trimEnd().split("\n").length, 6);
    assertMatch(output, /^[0-9a-f]{64}  tapid-1\.2\.3-aarch64-apple-darwin\.tar\.gz/m);
    await writeFile(join(directory, "unexpected.tar.gz"), "unexpected");
    await assertRejects(() => checksumLines(directory, "1.2.3"));
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
});

test("checksum generation streams archives sequentially", async () => {
  const helper = await text("tools/release/release.ts");
  assert(helper.includes("createReadStream"));
  assert(!helper.includes("Promise.all("));
  assert(!helper.includes("update(await readFile(path))"));
});

test("binary release builds the exact source and gates one complete candidate", async () => {
  const workflow = await text(".github/workflows/release-publication.yml");
  const coordinator = await text(".github/workflows/crates-publication.yml");
  for (const target of [
    "aarch64-apple-darwin", "aarch64-pc-windows-msvc", "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu",
  ]) assert(workflow.includes(`target: ${target}`));
  assert(workflow.includes("workflow_call:"));
  assert(!workflow.includes("\n  push:"));
  assert(!workflow.includes("softprops/action-gh-release"));
  assert(!workflow.includes("ref: ${{ inputs.commit_sha }}"));
  assert(workflow.includes("tools/release/verify-source.sh"));
  assert(workflow.includes('git checkout --detach "$verified_commit"'));
  assert(workflow.includes('test "$(git rev-parse HEAD)" = "$verified_commit"'));
  assert(workflow.includes('"staging/$BINARY" --version | grep -Fx "tapid $VERSION"'));
  assert(workflow.includes("pattern: tapid-*"));
  assert(workflow.includes("merge-multiple: true"));
  assert(workflow.includes("needs: [prepare, build]"));
  assert(workflow.includes("needs.build.result == 'success' || needs.build.result == 'skipped'"));
  assert(workflow.includes("environment: stable-release"));
  assert(workflow.includes("TAPID_RELEASE_SIGNING_KEY: ${{ secrets.TAPID_RELEASE_ED25519_PRIVATE_KEY }}"));
  assert(!workflow.includes("--clobber"));
  assert(!workflow.includes("gh release edit"));
  const checksums = workflow.indexOf("tools/release/release.ts checksums release");
  const metadata = workflow.indexOf("tools/release/release.ts metadata release");
  const bootstrap = workflow.indexOf("tools/release/bootstrap.ts release");
  const approval = workflow.indexOf("environment: stable-release");
  const unsigned = workflow.indexOf("tools/release/candidate.ts unsigned release");
  const recheck = workflow.indexOf("tools/release/candidate.ts validate-unsigned release");
  const sign = workflow.indexOf("tools/release/sign.ts sign release/");
  const verify = workflow.indexOf("tools/release/sign.ts verify release/");
  const retain = workflow.indexOf("name: Retain signed bytes before the first GitHub asset upload");
  const create = workflow.indexOf("tools/release/candidate.ts create-draft release");
  assert(checksums >= 0 && checksums < metadata && metadata < bootstrap && bootstrap < unsigned);
  assert(unsigned < approval && approval < recheck && recheck < sign && sign < verify && verify < retain && retain < create);
  assert(coordinator.includes("group: tapid-stable-release"));
  assert(coordinator.includes("cancel-in-progress: false"));
  assert(coordinator.includes("tools/release/candidate.ts select"));
  assert(coordinator.includes("uses: ./.github/workflows/release-draft-verify.yml"));
  assert(coordinator.includes("needs: [preflight, candidate, verify]"));
});

test("release runbook documents the workflow's eleven-asset contract", async () => {
  const workflow = await text(".github/workflows/release-publication.yml");
  const runbook = await text("docs/release-distribution.md");
  const assets = [
    "tapid-<version>-aarch64-apple-darwin.tar.gz",
    "tapid-<version>-x86_64-apple-darwin.tar.gz",
    "tapid-<version>-aarch64-unknown-linux-gnu.tar.gz",
    "tapid-<version>-x86_64-unknown-linux-gnu.tar.gz",
    "tapid-<version>-aarch64-pc-windows-msvc.tar.gz",
    "tapid-<version>-x86_64-pc-windows-msvc.tar.gz",
    "SHA256SUMS",
    "tapid-release-v1.tsv",
    "tapid-release-v1.tsv.sig",
    "install.sh",
    "install.ps1",
  ];
  for (const asset of assets) assert(runbook.includes(`\`${asset}\``), `runbook omits ${asset}`);
  assert(runbook.includes("upload and read back the exact eleven-asset set"));
  assert(runbook.includes("This is eleven assets for the new release flow."));
  assert(runbook.includes("Require exactly eleven assets and no unexpected names for a new release."));
  assert(runbook.includes("the exact eleven assets remain present for the new release"));
  assert(runbook.includes("Use eleven assets for the new flow or seven for a historical tagged workflow."));
  assert(!runbook.includes("exact seven-asset set"));
  assert(!runbook.includes("Require exactly seven assets and no unexpected names"));
  assert(workflow.includes("tools/release/candidate.ts create-draft release"));
});

test("release workflow uses Node.js 24 actions and the Visual Studio 2026 ARM runner", async () => {
  const workflow = await text(".github/workflows/release-publication.yml");
  for (const action of [
    "actions/checkout@d23441a48e516b6c34aea4fa41551a30e30af803 # v6",
    "actions/setup-node@820762786026740c76f36085b0efc47a31fe5020 # v7",
    "actions/upload-artifact@043fb46d1a93c77aae656e7c1c64a875d1fc6a0a # v7",
    "actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8",
  ]) assert(workflow.includes(action));
  for (const legacyAction of [
    "actions/checkout@v4",
    "actions/setup-node@v4",
    "actions/upload-artifact@v4",
    "actions/download-artifact@v4",
  ]) assert(!workflow.includes(legacyAction));
  assert(workflow.includes("runner: windows-11-vs2026-arm"));
  assert(!workflow.includes("runner: windows-11-arm"));
});

test("repository workflows avoid the deprecated Node.js 20 action majors", async () => {
  const workflows = await readdir(join(root, ".github/workflows"));
  assert(workflows.includes("ci.yml"));
  for (const name of workflows.filter((name: string) => name.endsWith(".yml"))) {
    const path = `.github/workflows/${name}`;
    const workflow = await text(path);
    for (const legacyAction of [
      "actions/checkout@v4",
      "actions/setup-node@v4",
      "actions/upload-artifact@v4",
      "actions/download-artifact@v4",
    ]) assert(!workflow.includes(legacyAction), `${path} still uses ${legacyAction}`);
  }
  const ci = await text(".github/workflows/ci.yml");
  assert(ci.includes("runner: windows-11-vs2026-arm"));
  assert(!ci.includes("runner: windows-11-arm"));
  assertEquals(
    ci.match(/persist-credentials: false/g)?.length,
    ci.match(/uses: actions\/checkout@d23441a48e516b6c34aea4fa41551a30e30af803/g)?.length,
  );
});

test("crates publication actions use immutable commit references", async () => {
  const workflow = await text(".github/workflows/crates-publication.yml");
  const actionLines = workflow.split(/\r?\n/).filter((line: string) => /^\s*(?:-\s*)?uses:/.test(line));
  assert(actionLines.length > 0);
  for (const line of actionLines) {
    if (line.includes("uses: ./")) assertMatch(line, /uses:\s+\.\/\.github\/workflows\/[a-z-]+\.yml$/);
    else assertMatch(line, /uses:\s+[^@\s]+@[a-f0-9]{40}(?:\s+#\s+[^\s]+)?$/);
  }
});

test("crates publication preserves OIDC identity and requires same-run release evidence", async () => {
  const workflow = await text(".github/workflows/crates-publication.yml");
  assert(workflow.includes("workflow_dispatch:"));
  assert(workflow.includes("branches: [main]"));
  assert(workflow.includes("paths: [docs/releases/intent.json]"));
  assert(!workflow.includes("workflow_run:"));
  assert(!workflow.includes("pull_request_target:"));
  assert(!workflow.includes("types: [published]"));
  assert(workflow.includes("id-token: write"));
  assert(workflow.includes("environment: crates-io-release"));
  assert(workflow.includes("rust-lang/crates-io-auth-action@c6f97d42243bad5fab37ca0427f495c86d5b1a18 # v1.0.5"));
  assert(workflow.includes("cargo package --workspace --locked"));
  assert(workflow.includes('tools/release/release.ts" current-version'));
  assert(workflow.includes('test "$SOURCE_VERSION" = "$VERSION"'));
  assert(!workflow.includes('check-tag "$TAG"'));
  assert(workflow.includes('git cat-file -t "refs/tags/$REQUESTED_TAG"'));
  assert(workflow.includes('git merge-base --is-ancestor "$SOURCE_SHA" origin/main'));
  assert(workflow.includes("if: github.ref == 'refs/heads/main'"));
  assert(workflow.includes("ref: ${{ github.sha }}"));
  assert(workflow.includes("needs: [preflight, candidate, verify, promote, smoke]"));
  assert(workflow.includes("uses: ./.github/workflows/release-public-smoke.yml"));
  assert(workflow.includes("release_id: ${{ needs.candidate.outputs.release_id }}"));
  assert(workflow.includes("commit_sha: ${{ needs.preflight.outputs.tag_commit }}"));
  assert(!workflow.includes("release-public-smoke.yml/runs?event=release"));
  assert(!workflow.includes(".display_title =="));
  assert(!workflow.includes("CARGO_REGISTRY_TOKEN: ${{ secrets."));
  const packageGate = workflow.indexOf("Verify reviewed crate plan and locked package compatibility");
  const publishJob = workflow.indexOf("\n  publish:");
  const candidateRecheck = workflow.indexOf("Recheck immutable public candidate after same-run smoke");
  const sourceRecheck = workflow.indexOf("Recheck exact source and remaining approved crate suffix");
  const auth = workflow.indexOf("Authenticate to crates.io with OIDC");
  const publish = workflow.indexOf("Publish missing crates one at a time");
  assert(packageGate >= 0 && packageGate < publishJob && publishJob < candidateRecheck);
  assert(candidateRecheck < sourceRecheck && sourceRecheck < auth && auth < publish);
  assert(workflow.slice(candidateRecheck,auth).includes("tools/release/candidate.ts publish"));
  assert(workflow.slice(candidateRecheck,auth).includes("tools/release/automation.ts check-plan"));
  assert(workflow.includes('test "$(git rev-parse "refs/tags/$TAG^{commit}")" = "$SOURCE_SHA"'));
  const publisher = await text("tools/release/publish.ts");
  assertEquals(publisher.match(/cwd: workspaceDir/g)?.length, 2);
  for (const contract of [
    '\"publish\", \"--no-verify\", \"--locked\"',
    "env: cargoMetadataEnv(process.env, cargoHome)", "env: cargoPublishEnv(process.env, token, cargoHome)",
    "CARGO_CHILD_ENV_KEYS", "metadata.lockfiles = await findCargoLockfiles(workspaceDir)",
    "const lockfileVerification = lockfiles", "cleanInstallVerification",
    "cargo install tapid --version ${tapidVersion}",
  ]) assert(publisher.includes(contract));
  assert(!publisher.includes("verifyPackage"));
  assert(!publisher.includes('"package", "--locked", "--package"'));
  assert(workflow.includes("CARGO_HOME: ${{ runner.temp }}/package-verify-cargo-home"));
  assert(workflow.includes("--publish --json --expected-plan"));
  assert(workflow.includes("CARGO_REGISTRY_TOKEN: ${{ steps.auth.outputs.token }}"));
  assert(workflow.includes("CARGO_HOME: ${{ runner.temp }}/clean-cargo-home"));
  assert(workflow.includes("for attempt in 1 2 3 4 5 6; do"));
  assert(workflow.includes('cargo install tapid --version "$VERSION" --locked --root "$INSTALL_ROOT" && break'));
  assert(workflow.includes('test "$attempt" -lt 6 || exit 1'));
  assert(workflow.includes("sleep $((attempt * 10))"));
});

/** Ensure PR smoke validates its exact head without becoming release evidence. */
test("PR published-binary smoke is exact-head, read-only and separate from release approval", async () => {
  const ci = await text(".github/workflows/ci.yml");
  const job = ci.match(/^  pr-published-binary-smoke:\n[\s\S]*?(?=^  [a-z][a-z-]*:|$(?![\s\S]))/m)?.[0];
  assert(job, "missing unprivileged PR published-binary job");
  assertMatch(ci, /\n  pull_request:\n/);
  assert(!ci.includes("pull_request_target:"));
  assert(job.includes("if: github.event_name == 'pull_request'"));
  assert(job.includes("contents: read"));
  assert(job.includes("os: [ubuntu-latest, macos-latest, windows-latest]"));
  assert(job.includes("ref: ${{ github.event.pull_request.head.sha }}"));
  assert(job.includes("persist-credentials: false"));
  assert(job.includes("EXPECTED_HEAD: ${{ github.event.pull_request.head.sha }}"));
  assert(job.includes("$actual -cne $env:EXPECTED_HEAD"));
  assert(job.includes("RELEASE_TAG: v0.0.10"));
  assert(job.includes("RELEASE_SOURCE_SHA: 3d5f97c91f08b64a5ace26c2004081d57b88fee2"));
  for (const forbidden of ["secrets.", "github.token", ": write", "upload-artifact", "download-artifact", "environment:", "continue-on-error", "cargo build", "releases/latest"]) {
    assert(!job.includes(forbidden), `PR smoke must not contain ${forbidden}`);
  }
  for (const variable of ["HOME", "USERPROFILE", "LOCALAPPDATA", "XDG_CACHE_HOME", "TMPDIR", "TEMP", "TMP"]) {
    assert(job.includes(`\"${variable}=`), `missing isolated ${variable}`);
  }
  assert(job.includes("--proto-redir '=https' --tlsv1.2 --max-time 60 --max-filesize 262144"));
  assert(job.includes('sh "$RUNNER_TEMP/pr-published/install.sh" --version "$RELEASE_TAG"'));
  assert(job.includes("& $installer -Version $env:RELEASE_TAG -Repo LimeTip/tapid -InstallDir $installDir"));
  assert(job.includes("finally {"));
  assert(job.includes("SetEnvironmentVariable('Path', $originalUserPath, 'User')"));
  assert(job.includes("if ($actual -cne 'tapid 0.0.10')"));
  assert(job.includes("node tests/fixtures/create_consumer_project.js"));
  assert(job.includes("node tests/fixtures/validate_consumer_project.js --binary $binary --release-tag $env:RELEASE_TAG"));
  assert(job.includes("pre-merge regression evidence only"));
});

test("public smoke tests use the published installer and released version", async () => {
  const workflow = await text(".github/workflows/release-public-smoke.yml");
  assert(workflow.includes("types: [published]"));
  // Keep the tagged installer evidence while checking the live website copies too.
  assert(workflow.includes('installer_url="$INSTALLER_URL"'));
  assert(workflow.includes('$installerUrl = $env:INSTALLER_URL'));
  assert(workflow.includes("https://tapid.dev/install.sh"));
  assert(workflow.includes("https://tapid.dev/install.ps1"));
  assert(workflow.includes('"$installer_url" -o "$RUNNER_TEMP/install.sh"'));
  assert(workflow.includes('$installerUrl --output $installer'));
  assert(workflow.includes('sh "$RUNNER_TEMP/install.sh" --version "$RELEASE_TAG"'));
  assert(workflow.includes('& $installer -Version $env:RELEASE_TAG'));
  assert(workflow.includes("github.event.release.tag_name"));
  assert(workflow.includes("--version"));
  assert(workflow.includes("Install latest release through discovery"));
  assert(workflow.includes("shell: powershell"));
  assert(workflow.includes('test "$actual" = "tapid ${RELEASE_TAG#v}"'));
});

test("public Unix upgrade binds selected source and independent latest destination and retains failures", async () => {
  const workflow = await text(".github/workflows/release-public-smoke.yml");
  const unix = workflow.slice(workflow.indexOf("  unix:"), workflow.indexOf("  windows:"));
  const latest = unix.indexOf("- name: Install latest release through discovery");
  const upgrade = unix.indexOf("- name: Run canonical published upgrade");
  const retention = unix.indexOf("- name: Retain published upgrade evidence");
  assert(upgrade > latest && latest >= 0, "upgrade must follow independent latest installation");
  assert(retention > upgrade, "upgrade report must be retained after execution");
  const step = unix.slice(upgrade, retention);
  assert(step.includes('timeout-minutes: 5'));
  assert(step.includes('--lane published --example upgrade'));
  assert(step.includes('binary="$RUNNER_TEMP/tapid/tapid"'));
  assert(step.includes('target="$RUNNER_TEMP/tapid-latest/tapid"'));
  assert(step.includes('--upgrade-target-sha256 "$target_digest"'));
  assert(step.includes('--upgrade-target-version "tapid ${LATEST_TAG#v}"'));
  assert(step.includes('--expected-sha256 "$digest"'));
  assert(step.includes('--expected-version "tapid ${RELEASE_TAG#v}"'));
  assert(step.includes('--release-tag "$RELEASE_TAG" --release-source-sha "$RELEASE_SHA"'));
  assert(step.includes('--allow-network'));
  assert(step.includes('--report "$RUNNER_TEMP/doc-contract-upgrade.json"'));
  assert(step.includes('id: upgrade'));
  assert(unix.slice(retention).includes("if: ${{ always() && steps.upgrade.outcome != 'skipped' }}"));
  assert(unix.slice(retention).includes('if-no-files-found: error'));
  assert(unix.slice(retention).includes('${{ runner.temp }}/doc-contract-upgrade.json'));
  assert(!workflow.includes('contents: write'));
  assert(!workflow.includes('id-token: write'));
  assert(!workflow.includes('continue-on-error: true'));
});

test("public installers exercise explicit and latest discovery plus supported upgrades", async () => {
  const workflow = await text(".github/workflows/release-public-smoke.yml");
  assert(workflow.includes("--limit 100 --json tagName,isDraft,isPrerelease"));
  assert(workflow.includes("(0, 0, 10) <= version(r['tagName']) < latest"));
  const unix = workflow.slice(workflow.indexOf("  unix:"), workflow.indexOf("  windows:"));
  const windows = workflow.slice(workflow.indexOf("  windows:"));
  for (const job of [unix, windows]) {
    assert(job.includes("Check the public website installer with an explicit version"));
    assert(job.includes("Check previous-version upgrade and repeat upgrade through the public service"));
    assert(job.includes("PREVIOUS_TAG: ${{ needs.resolve.outputs.previous_tag }}"));
    assert(job.includes("LATEST_TAG: ${{ needs.resolve.outputs.latest_tag }}"));
    assert(job.includes("RECORD_AWARE: ${{ needs.resolve.outputs.record_aware }}"));
    assert(job.includes("is already up to date"));
    assert(job.includes("public-repeat-upgrade.txt"));
    assert(job.includes("Skip truthful repeat assertion: releases through 0.0.10"));
  }
  assert(unix.includes('latest_installer_url="$LATEST_INSTALLER_URL"'));
  const parity = unix.indexOf('cmp "$RUNNER_TEMP/public-install.sh" "$RUNNER_TEMP/latest-tag-install.sh"');
  assert(parity >= 0 && parity < unix.indexOf('sh "$RUNNER_TEMP/public-install.sh" --version'));
  assert(windows.includes('$latestInstallerUrl = $env:LATEST_INSTALLER_URL'));
  const nativePublic = windows.slice(windows.indexOf("- name: Check the public website installer with an explicit version"));
  const nativeParity = nativePublic.indexOf("if ($installerDigest -cne $latestInstallerDigest)");
  assert(nativeParity >= 0 && nativeParity < nativePublic.indexOf("& $installer -Version"));
  assert(unix.includes('sh "$RUNNER_TEMP/public-install.sh" --version "$RELEASE_TAG"'));
  assert(unix.includes('sh "$RUNNER_TEMP/public-install.sh" --install-dir "$install_dir"'));
  assert(unix.includes(`test "$(shasum -a 256 "$binary" | cut -d ' ' -f 1)" = "$expected_digest"`));
  assert(windows.includes("$installer = Join-Path $env:RUNNER_TEMP 'public-install.ps1'"));
  assert(windows.includes("& $installer -Version $env:RELEASE_TAG -InstallDir $installDir"));
  assert(windows.includes("& $installer -InstallDir $installDir"));
  assert(windows.includes("if ((Get-FileHash $binary).Hash -cne $expectedDigest)"));
});

test("public smoke validates ancestry before detaching the resolved trusted runner", async () => {
  const workflow = await text(".github/workflows/release-public-smoke.yml");
  const unix = workflow.slice(workflow.indexOf("  unix:"), workflow.indexOf("  windows:"));
  const windows = workflow.slice(workflow.indexOf("  windows:"));
  for (const job of [unix, windows]) {
    const checkouts = [...job.matchAll(/uses: actions\/checkout@(\S+)/g)];
    assertEquals(checkouts.length, 1);
    assertEquals(checkouts[0][1], "d23441a48e516b6c34aea4fa41551a30e30af803");
    assertMatch(job, /ref: main\n\s+fetch-depth: 0/);
    assert(job.includes("persist-credentials: false"));
    assert(!job.includes("ref: ${{"));
  }
  const bashValidation = '[[ "$EXPECTED_RUNNER_SHA" =~ ^[a-f0-9]{40}$ ]]';
  const bashAncestry = 'git merge-base --is-ancestor "$EXPECTED_RUNNER_SHA" HEAD';
  const bashDetach = 'git checkout --detach "$EXPECTED_RUNNER_SHA"';
  const psValidation = "if ($env:EXPECTED_RUNNER_SHA -cnotmatch '\\A[a-f0-9]{40}\\z')";
  const psAncestry = 'git merge-base --is-ancestor $env:EXPECTED_RUNNER_SHA HEAD';
  const psDetach = 'git checkout --detach $env:EXPECTED_RUNNER_SHA';
  for (const [job, validation, ancestry, detach, execution] of [
    [unix, bashValidation, bashAncestry, bashDetach, 'sh "$RUNNER_TEMP/install.sh"'],
    [windows, psValidation, psAncestry, psDetach, '& $installer -Version'],
  ]) {
    assert(job.indexOf(validation) >= 0);
    assert(job.indexOf(validation) < job.indexOf(ancestry));
    assert(job.indexOf(ancestry) < job.indexOf(detach));
    assert(job.indexOf(detach) < job.indexOf(execution));
  }
  for (const command of [psAncestry, psDetach]) {
    assert(windows.includes(`${command}\n          if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }`));
  }
});

test("published documentation invocation opts into network and uses an installed binary prerequisite", async () => {
  const workflow = await text(".github/workflows/release-public-smoke.yml");
  assertMatch(workflow, /& \.\/scripts\/check-doc-examples\.ps1 -AllowNetwork -Binary /);
  const contracts = JSON.parse(await text("docs/examples/contracts.json"));
  const help = contracts.examples.find((example: { id: string }) => example.id === "upgrade-help");
  assert(help.prerequisites.includes("installed-tapid"));
  assert(!help.prerequisites.includes("source-built-tapid"));
});

test("public smoke retains tagged installer provenance before execution on both platforms", async () => {
  const workflow = await text(".github/workflows/release-public-smoke.yml");
  const unixStart = workflow.indexOf("  unix:");
  const windowsStart = workflow.indexOf("  windows:");
  assert(unixStart >= 0, "missing Unix job section");
  assert(windowsStart >= 0, "missing Windows job section");
  assert(windowsStart > unixStart, "Windows job section must follow Unix job section");
  const unix = workflow.slice(unixStart, windowsStart);
  const windows = workflow.slice(windowsStart);
  for (const [job, execution, script] of [
    [unix, 'sh "$RUNNER_TEMP/install.sh" --version', 'install.sh'],
    [windows, '& $installer -Version', 'install.ps1'],
  ]) {
    const provenance = job.indexOf("installer-provenance.txt");
    assert(provenance >= 0 && provenance < job.indexOf(execution),
      `${script}: record provenance even if installer execution fails`);
    for (const field of ["installer_url=", "release_tag=", "release_source_sha=", "installer_sha256="]) {
      assert(job.includes(field), `${script}: missing ${field}`);
    }
    const uploadStart = job.indexOf("uses: actions/upload-artifact@");
    assert(uploadStart >= 0, `${script}: missing upload-artifact step`);
    const upload = job.slice(uploadStart);
    for (const file of ["installer-provenance.txt", "installer-sha256.txt", script]) {
      assert(upload.includes('${{ runner.temp }}/' + file), `${script}: not retaining ${file}`);
    }
    assert(job.includes("if: always()"));
    assert(job.includes("--max-time 60 --max-filesize 262144"));
    assert(job.includes("--proto-redir '=https'"));
  }
  assert(unix.includes('shasum -a 256 "$RUNNER_TEMP/install.sh"'));
  assert(windows.includes('Get-FileHash -Algorithm SHA256 -LiteralPath $installer'));
});

test("installers verify signed release records before parsing", async () => {
  for (const path of ["scripts/install.sh", "scripts/install.ps1"]) {
    const installer = await text(path);
    const checksum = installer.indexOf("SHA256SUMS");
    const archiveDownload = path.endsWith(".sh")
      ? installer.indexOf('"$archive_url" -o')
      : installer.indexOf('Save-BoundedHttpsFile $archiveUrl');
    assert(checksum >= 0 && archiveDownload > checksum);
    assert(!installer.includes("release-manifest.json"));
    assert(installer.includes("release.tsv.sig"));
    assert(installer.includes("release record signature verification failed"));
  }
  const verifier = await text("crates/tapid-cli/src/application/release_verification.rs");
  assert(verifier.includes("verify_signature"));
  assert(verifier.includes("KeyRing::production()"));
  const shell = await text("scripts/install.sh");
  assert(shell.includes("release archive must contain exactly one member named tapid"));
  assert(shell.includes("MAX_ARCHIVE_BYTES="));
  assert(shell.includes("MAX_BINARY_BYTES="));
  assert(shell.includes("tar -xOzf"));
  assert(shell.includes('[ "$INSTALL_DIR" = "$HOME/.local/bin" ] || return 0'));
  assert(shell.includes("configure_path || fail 'could not safely update the selected shell startup file'"));
  assert(!shell.includes('mv -f "$STAGED_BINARY" "$INSTALL_DIR/tapid"; STAGED_BINARY=""\n  mv -f "$STAGED_MARKER"'));
  assert(!shell.includes('mv -f "$STAGED_BINARY" "$INSTALL_DIR/tapid"; STAGED_BINARY=""\nmv -f "$STAGED_MARKER"'));
  const powershell = await text("scripts/install.ps1");
  assert(powershell.includes("RuntimeInformation]::OSArchitecture"));
  assert(powershell.includes("$members.Count -ne 1"));
  assert(powershell.includes("$MAX_ARCHIVE_BYTES"));
  assert(powershell.includes("$MAX_BINARY_BYTES"));
  assert(powershell.includes("Save-BoundedHttpsFile"));
  assert(powershell.includes("Add-Type -AssemblyName System.Net.Http"));
  assert(powershell.includes("$handler.AllowAutoRedirect = $false"));
  assert(powershell.includes("redirect target must use HTTPS"));
  assert(powershell.includes("too many redirects"));
  const destinationDispose = powershell.indexOf("$destinationStream.Dispose()");
  const failedDownloadCleanup = powershell.indexOf("if ($downloadError -or $cleanupError) { Remove-Item -LiteralPath $Path");
  const primaryRethrow = powershell.indexOf("if ($downloadError) { throw $downloadError }");
  assert(destinationDispose >= 0 && failedDownloadCleanup > destinationDispose && primaryRethrow > failedDownloadCleanup);
  assert(powershell.includes("uncompressed size"));
  assert(powershell.includes("tar.exe -tvzf"));
  assert(!powershell.includes("TAPID_TEST_FIXTURE"));
  assert(!powershell.includes("IsPathFullyQualified"));
  assert(powershell.includes("Test-AbsolutePath"));
  assert(powershell.includes('Write-Warning "Tapid was installed, but the user PATH could not be updated'));
  assert(!powershell.includes('Move-Item -LiteralPath $staged -Destination $destination -Force\n        Move-Item -LiteralPath $stagedMarker'));
  assert(!powershell.includes('Move-Item -LiteralPath $staged -Destination $destination -Force\n    Move-Item -LiteralPath $stagedMarker'));
  const discoveryCatch = powershell.indexOf('catch { Fail "could not contact the stable release discovery endpoint" }');
  const resolvedUri = powershell.indexOf("$resolvedUri = $discovery.BaseResponse.ResponseUri");
  assert(discoveryCatch >= 0 && discoveryCatch < resolvedUri);
  assert(powershell.includes("$discovery.BaseResponse.RequestMessage.RequestUri"));
  const powershellUninstaller = await text("scripts/uninstall.ps1");
  assert(powershellUninstaller.includes("Test-AbsolutePath"));
  assert(powershellUninstaller.includes(".tapid-managed"));
  assert(powershellUninstaller.includes("tapid-managed-v1`n"));
  assert(powershellUninstaller.includes("refusing foreign install marker"));
  assert(powershellUninstaller.includes("Remove-Item -LiteralPath $marker -Force"));
  assert(!powershellUninstaller.includes("IsPathRooted"));
});

test("Unix installer rejects multiline repository and version values", async () => {
  const installer = join(root, "scripts/install.sh");
  const installDir = await mkdtemp(join(tmpdir(), "tapid-installer-input-"));
  try {
    await assertRejects(
      () => execFileAsync("sh", [installer, "--repo", "LimeTip/tapid\nother", "--version", "invalid", "--install-dir", installDir]),
      (error: any) => error.stderr.includes("repository must be OWNER/REPO"),
    );
    await assertRejects(
      () => execFileAsync("sh", [installer, "--repo", "not-a-repository", "--version", "invalid", "--install-dir", installDir]),
      (error: any) => error.stderr.includes("repository must be OWNER/REPO"),
    );
    await assertRejects(
      () => execFileAsync("sh", [installer, "--version", "v1.2.3\nother", "--install-dir", installDir]),
      (error: any) => error.stderr.includes("version must be a stable release"),
    );
  } finally {
    await rm(installDir, { recursive: true, force: true });
  }
});

test("PowerShell installer rejects multiline repository and version values", async (context) => {
  try {
    await execFileAsync("pwsh", ["-NoProfile", "-Command", "$null"]);
  } catch {
    context.skip("PowerShell is unavailable");
    return;
  }
  const installer = join(root, "scripts/install.ps1");
  const installDir = await mkdtemp(join(tmpdir(), "tapid-powershell-input-"));
  try {
    await assertRejects(
      () => execFileAsync("pwsh", ["-NoProfile", "-File", installer, "-Repo", "LimeTip/tapid\nother", "-Version", "invalid", "-InstallDir", installDir]),
      (error: any) => error.stderr.includes("repository must be OWNER/REPO"),
    );
    await assertRejects(
      () => execFileAsync("pwsh", ["-NoProfile", "-File", installer, "-Version", "v1.2.3\n", "-InstallDir", installDir]),
      (error: any) => error.stderr.includes("version must be a stable release"),
    );
    await assertRejects(
      () => execFileAsync("pwsh", ["-NoProfile", "-File", installer, "-Version", "v1.2.3", "-InstallDir", "relative-path"]),
      (error: any) => error.stderr.includes("install directory must be an absolute path"),
    );
  } finally {
    await rm(installDir, { recursive: true, force: true });
  }
});

test("Unix installer preserves a valid install when an unsafe archive is rejected", async () => {
  const fixture = await mkdtemp(join(tmpdir(), "tapid-installer-fixture-"));
  const installDir = join(fixture, "installed");
  const payload = join(fixture, "payload");
  const fakeBin = join(fixture, "bin");
  const version = "1.2.3";
  const target = platform === "darwin"
    ? (arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin")
    : (arch === "arm64" ? "aarch64-unknown-linux-gnu" : "x86_64-unknown-linux-gnu");
  const archive = `tapid-${version}-${target}.tar.gz`;
  try {
    await mkdir(payload);
    await mkdir(fakeBin);
    await writeFile(join(payload, "tapid"), "#!/bin/sh\nprintf 'tapid 1.2.3\\n'\n");
    await chmod(join(payload, "tapid"), 0o755);
    await execFileAsync("tar", ["-czf", join(fixture, archive), "-C", payload, "tapid"]);
    const writeChecksums = async () => {
      const digest = createHash("sha256").update(await readFile(join(fixture, archive))).digest("hex");
      await writeFile(join(fixture, "SHA256SUMS"), `${digest}  ${archive}\n`);
    };
    await writeChecksums();
    const fakeCurl = join(fakeBin, "curl");
    await writeFile(fakeCurl, `#!/bin/sh
set -eu
out=''
url=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    https://*) url="$1"; shift ;;
    --max-filesize) shift 2 ;;
    *) shift ;;
  esac
done
cp "$TAPID_TEST_FIXTURE/\${url##*/}" "$out"
`);
    await chmod(fakeCurl, 0o755);
    const env = { ...process.env, PATH: `${fakeBin}:${process.env.PATH}`, TAPID_TEST_FIXTURE: fixture, TAPID_RELEASE_BASE_URL: "https://github.com/LimeTip/tapid/releases/download" };
    const installer = join(root, "scripts/install.sh");
    await execFileAsync("sh", [installer, "--version", version, "--install-dir", installDir], { env });
    assertEquals((await execFileAsync(join(installDir, "tapid"), ["--version"])).stdout.trim(), "tapid 1.2.3");

    await writeFile(join(payload, "extra"), "unsafe");
    await execFileAsync("tar", ["-czf", join(fixture, archive), "-C", payload, "tapid", "extra"]);
    await writeChecksums();
    await assertRejects(
      () => execFileAsync("sh", [installer, "--version", version, "--install-dir", installDir], { env }),
      (error: any) => error.stderr.includes("exactly one member named tapid"),
    );
    assertEquals((await execFileAsync(join(installDir, "tapid"), ["--version"])).stdout.trim(), "tapid 1.2.3");
  } finally {
    await rm(fixture, { recursive: true, force: true });
  }
});

test("CI runs the TypeScript tool suite", async () => {
  const workflow = await text(".github/workflows/ci.yml");
  assert(workflow.includes("actions/setup-node@820762786026740c76f36085b0efc47a31fe5020"));
  const suite = workflow.split(/\r?\n/).find(line => line.includes("run: node --experimental-strip-types --test"));
  assert(suite);
  assert(suite.includes("tools/check_architecture_test.ts"));
  assert(suite.includes("tools/release/*_test.ts"), "CI must discover every release helper's tests");
});


test("native Windows archive fixture refreshes release records before each install", async () => {
  const workflow = await text(".github/workflows/ci.yml");
  const fixture = workflow.slice(workflow.indexOf("  windows-installer-contract:"), workflow.indexOf("  package:"));
  assert(fixture.includes('function Write-FixtureReleaseRecord'));
  assert(fixture.includes('name: Windows installer contract'));
  assert(fixture.includes('actions/download-artifact@3e5f45b2cfb9172054b4087a40e8e0b5a5461e7c # v8'));
  assert(fixture.includes('digest-mismatch: error'));
  assert(!fixture.includes('rust-toolchain@') && !fixture.includes('cargo build'));
  const verified = fixture.indexOf('tools/release/ci_binary.ts verify');
  const execute = fixture.indexOf('$actual = & target/debug/tapid.exe --version');
  const install = fixture.indexOf('name: Install and reject archive fixtures');
  assert(verified >= 0 && verified < execute && execute < install);
  assert(fixture.includes("Copy-Item -LiteralPath (Join-Path $PWD 'target/debug/tapid.exe')"));
  assertEquals(fixture.match(/^          Write-FixtureReleaseRecord$/gm)?.length, 2);
  assert(fixture.includes('tapid-release-v1`t1.2.3'));
  assert(fixture.includes('$size = (Get-Item -LiteralPath $archive).Length'));
  assert(fixture.includes("'https://tapid.dev/releases/v1/v1.2.3.tsv'"));
  assert(fixture.includes('https://gitlab.example/tapid/releases/v1.2.3/downloads/$archiveName'));
  assert(fixture.includes('fixture did not use release record discovery and artifact URLs'));
  assert(!fixture.includes('SHA256SUMS'));
  assert(fixture.includes('exactly one member named tapid.exe'));
});
