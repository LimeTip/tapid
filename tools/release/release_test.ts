import {
  match as assertMatch,
  ok as assert,
  rejects as assertRejects,
  strictEqual as assertEquals,
  throws as assertThrows,
} from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, rm, truncate, writeFile } from "node:fs/promises";
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

test("binary release follows the small draft release flow", async () => {
  const workflow = await text(".github/workflows/release-publication.yml");
  for (const target of [
    "aarch64-apple-darwin",
    "aarch64-pc-windows-msvc",
    "aarch64-unknown-linux-gnu",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
  ]) assert(workflow.includes(`target: ${target}`));
  assert(workflow.includes("tags:"));
  assert(workflow.includes('"v*.*.*"'));
  assert(!workflow.includes("softprops/action-gh-release"));
  assert(workflow.includes('gh api --paginate "repos/$GITHUB_REPOSITORY/releases"'));
  assert(workflow.includes('gh api --method POST "repos/$GITHUB_REPOSITORY/releases"'));
  const createRelease = workflow.indexOf('release_fields="$(gh api --method POST');
  const boundedReleaseReadback = workflow.indexOf("for attempt in 1 2 3 4 5; do", createRelease);
  const exactReleaseCount = workflow.indexOf(')" = 1 &&', boundedReleaseReadback);
  const exactReleaseId = workflow.indexOf(')" = "$release_id"; then', exactReleaseCount);
  assert(createRelease >= 0 && boundedReleaseReadback > createRelease);
  assert(boundedReleaseReadback < exactReleaseCount && exactReleaseCount < exactReleaseId);
  assert(workflow.includes('[ "$attempt" = 5 ] || sleep 2'));
  assert(workflow.includes("release read-back did not converge"));
  assert(workflow.includes('expected_upload_url="https://uploads.github.com/repos/$GITHUB_REPOSITORY/releases/$release_id/assets{?name,label}"'));
  assert(workflow.includes('UPLOAD_URL: ${{ steps.release.outputs.upload_url }}'));
  assert(workflow.includes('"$UPLOAD_URL?name=$name"'));
  assert(workflow.includes('gh api "repos/$GITHUB_REPOSITORY/releases/$release_id" --jq .draft'));
  assert(workflow.includes("-F draft=true"));
  assert(!workflow.includes('gh release upload "$GITHUB_REF_NAME"'));
  assert(!workflow.includes("--clobber"));
  const deriveTag = workflow.indexOf('RELEASE_TAG="v$(node --experimental-strip-types tools/release/release.ts current-version)"');
  assert(workflow.includes("set -euo pipefail"));
  const validateTagInput = workflow.indexOf('[[ "$RELEASE_TAG" =~ ^v(0|[1-9][0-9]*)');
  const deleteCheckoutTag = workflow.indexOf('git tag -d "$RELEASE_TAG"');
  const fetchAnnotatedTag = workflow.indexOf('refs/tags/$RELEASE_TAG:refs/tags/$RELEASE_TAG');
  const validateAnnotatedTag = workflow.indexOf('git cat-file -t "refs/tags/$RELEASE_TAG"');
  const fetchMain = workflow.indexOf("refs/heads/main:refs/remotes/origin/main");
  const ancestry = workflow.indexOf("merge-base --is-ancestor");
  const checkoutVerifiedTag = workflow.indexOf('git checkout --detach "$tag_commit"');
  const checkTag = workflow.indexOf('version="$(node --experimental-strip-types tools/release/release.ts check-tag "$RELEASE_TAG")"');
  assert(deriveTag >= 0 && deriveTag < validateTagInput);
  assert(validateTagInput < deleteCheckoutTag);
  assert(deleteCheckoutTag < fetchAnnotatedTag && fetchAnnotatedTag < validateAnnotatedTag);
  assert(validateAnnotatedTag < fetchMain && fetchMain < ancestry);
  assert(ancestry < checkoutVerifiedTag && checkoutVerifiedTag < checkTag);
  assert(workflow.includes('tag_commit="$(git rev-parse "refs/tags/$RELEASE_TAG^{commit}")"'));
  assert(workflow.includes('version="$(node --experimental-strip-types tools/release/release.ts check-tag "$RELEASE_TAG")"'));
  assert(!workflow.includes('ref: ${{ needs.prepare.outputs.tag_commit }}'));
  assertEquals(workflow.match(/git checkout --detach "\$TAG_COMMIT"/g)?.length, 2);
  assertEquals(workflow.match(/test "\$\(git rev-parse HEAD\)" = "\$TAG_COMMIT"/g)?.length, 2);
  assert(workflow.includes("unexpected draft release assets"));
  assert(workflow.includes("actions/download-artifact@v8"));
  assert(workflow.includes("workflow_dispatch:"));
  assert(workflow.includes("if: github.event_name == 'workflow_dispatch'"));
  assert(workflow.includes("ref: main"));
  assert(!workflow.includes("inputs:"));
  assert(!workflow.includes("${{ inputs."));
  assert(workflow.includes("group: release-publication"));
  assert(!workflow.includes("release-manifest"));
  assert(!workflow.includes("python"));
  assert(!workflow.includes("gh release edit"));
  const metadata = workflow.indexOf('tools/release/release.ts metadata release "$VERSION" "https://github.com/$GITHUB_REPOSITORY/releases/download/$RELEASE_TAG"');
  assert(metadata > workflow.indexOf("tools/release/release.ts checksums release"));
  assert(metadata < createRelease, "the complete metadata asset must exist before draft creation");
  assert(workflow.includes("for asset in release/*.tar.gz release/SHA256SUMS release/tapid-release-v1.tsv; do"));
  assert(workflow.includes("find release -maxdepth 1 -type f -exec basename {}"), "draft readback must include the metadata asset");
});

test("release workflow uses Node.js 24 actions and the Visual Studio 2026 ARM runner", async () => {
  const workflow = await text(".github/workflows/release-publication.yml");
  for (const action of [
    "actions/checkout@v6",
    "actions/setup-node@v7",
    "actions/upload-artifact@v7",
    "actions/download-artifact@v8",
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
  for (const path of [
    ".github/workflows/ci.yml",
    ".github/workflows/crates-publication.yml",
    ".github/workflows/release-publication.yml",
    ".github/workflows/website-installer-sync.yml",
  ]) {
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

test("crates publication uses trusted publishing and native Cargo", async () => {
  const workflow = await text(".github/workflows/crates-publication.yml");
  assert(workflow.includes("workflow_dispatch:"));
  assert(workflow.includes("tag:"));
  assert(!workflow.includes("types: [published]"));
  assert(workflow.includes("id-token: write"));
  assert(workflow.includes("environment: crates-io-release"));
  assert(workflow.includes("rust-lang/crates-io-auth-action@v1"));
  assert(workflow.includes("cargo package --workspace --locked"));
  assert(workflow.includes("node --experimental-strip-types tools/release/publish.ts"));
  assert(workflow.includes('check-tag "$TAG"'));
  assert(!workflow.includes('check-tag "${{ inputs.tag }}"'));
  const ancestry = workflow.indexOf('merge-base --is-ancestor "$TAG_COMMIT" refs/remotes/origin/main');
  const setupNode = workflow.indexOf("actions/setup-node@v7");
  const repositoryCode = workflow.indexOf("tools/release/release.ts");
  assert(ancestry >= 0 && ancestry < setupNode && ancestry < repositoryCode);
  assert(workflow.includes("git cat-file -t \"refs/tags/$TAG\""));
  assert(workflow.includes(".head_sha == env.TAG_COMMIT"));
  assert(workflow.includes("isDraft,isPrerelease"));
  assert(workflow.includes("release-public-smoke.yml"));
  assert(workflow.includes('.display_title == ("Public installer smoke " + env.TAG)'));
  assert(!workflow.includes(".head_branch == env.TAG"));
  assert(workflow.includes('actions/runs/$run_id/jobs'));
  assert(workflow.includes('test "$successful_jobs" -eq 3'));
  assert(!workflow.includes("python"));
  assert(!workflow.includes("CARGO_REGISTRY_TOKEN: ${{ secrets."));
});

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
  assert(workflow.includes('installer_url="https://raw.githubusercontent.com/LimeTip/tapid/$RELEASE_TAG/scripts/install.sh"'));
  assert(workflow.includes('$installerUrl = "https://raw.githubusercontent.com/LimeTip/tapid/$env:RELEASE_TAG/scripts/install.ps1"'));
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
  assert(unix.includes('latest_installer_url="https://raw.githubusercontent.com/LimeTip/tapid/$LATEST_TAG/scripts/install.sh"'));
  const parity = unix.indexOf('cmp "$RUNNER_TEMP/public-install.sh" "$RUNNER_TEMP/latest-tag-install.sh"');
  assert(parity >= 0 && parity < unix.indexOf('sh "$RUNNER_TEMP/public-install.sh" --version'));
  assert(windows.includes('$latestInstallerUrl = "https://raw.githubusercontent.com/LimeTip/tapid/$env:LATEST_TAG/scripts/install.ps1"'));
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

test("installers use checksums without embedded release signing", async () => {
  for (const path of ["scripts/install.sh", "scripts/install.ps1"]) {
    const installer = await text(path);
    const checksum = installer.indexOf("SHA256SUMS");
    const archiveDownload = path.endsWith(".sh")
      ? installer.indexOf('"$archive_url" -o')
      : installer.indexOf('Save-BoundedHttpsFile $archiveUrl');
    assert(checksum >= 0 && archiveDownload > checksum);
    assert(!installer.includes("release-manifest.json"));
    assert(!installer.includes("python"));
    assert(!installer.includes("Ed25519"));
  }
  const shell = await text("scripts/install.sh");
  assert(shell.includes("release archive must contain exactly one member named tapid"));
  assert(shell.includes("MAX_ARCHIVE_BYTES="));
  assert(shell.includes("MAX_BINARY_BYTES="));
  assert(shell.includes("tar -xOzf"));
  assert(shell.includes('[ "$INSTALL_DIR" = "$HOME/.local/bin" ] || return 0'));
  assert(shell.includes("configure_path || printf 'Tapid was installed, but PATH could not be updated."));
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
  assert(workflow.includes("node --experimental-strip-types --test tools/check_architecture_test.ts tools/release/release_test.ts tools/release/publish_test.ts"));
});


test("native Windows archive fixture refreshes release records before each install", async () => {
  const workflow = await text(".github/workflows/ci.yml");
  const fixture = workflow.slice(workflow.indexOf("  windows-installer-contract:"), workflow.indexOf("  package:"));
  assert(fixture.includes('function Write-FixtureReleaseRecord'));
  assertEquals(fixture.match(/^          Write-FixtureReleaseRecord$/gm)?.length, 2);
  assert(fixture.includes('tapid-release-v1`t1.2.3'));
  assert(fixture.includes('$size = (Get-Item -LiteralPath $archive).Length'));
  assert(fixture.includes("'https://tapid.dev/releases/v1/v1.2.3.tsv'"));
  assert(fixture.includes('https://gitlab.example/tapid/releases/v1.2.3/downloads/$archiveName'));
  assert(fixture.includes('fixture did not use release record discovery and artifact URLs'));
  assert(!fixture.includes('SHA256SUMS'));
  assert(fixture.includes('exactly one member named tapid.exe'));
});
