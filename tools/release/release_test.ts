import {
  match as assertMatch,
  ok as assert,
  rejects as assertRejects,
  strictEqual as assertEquals,
  throws as assertThrows,
} from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { arch, platform } from "node:process";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { promisify } from "node:util";
import { checksumLines, releaseVersion } from "./release.ts";

const root = fileURLToPath(new URL("../../", import.meta.url));
const text = (path: string) => readFile(join(root, path), "utf8");
const execFileAsync = promisify(execFile);

test("release tag must be stable semver and match tapid", () => {
  assertEquals(releaseVersion("v1.2.3", "1.2.3"), "1.2.3");
  for (const tag of ["1.2.3", "v1.2", "v1.2.3-rc.1", "main"]) {
    assertThrows(() => releaseVersion(tag, "1.2.3"));
  }
  assertThrows(() => releaseVersion("v1.2.3", "1.2.4"));
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
  assert(workflow.includes('expected_upload_url="https://uploads.github.com/repos/$GITHUB_REPOSITORY/releases/$release_id/assets{?name,label}"'));
  assert(workflow.includes('UPLOAD_URL: ${{ steps.release.outputs.upload_url }}'));
  assert(workflow.includes('"$UPLOAD_URL?name=$name"'));
  assert(workflow.includes('gh api "repos/$GITHUB_REPOSITORY/releases/$release_id" --jq .draft'));
  assert(workflow.includes("-F draft=true"));
  assert(!workflow.includes('gh release upload "$GITHUB_REF_NAME"'));
  assert(!workflow.includes("--clobber"));
  const deriveTag = workflow.indexOf('RELEASE_TAG="v$(node --experimental-strip-types tools/release/release.ts current-version)"');
  const validateTagInput = workflow.indexOf('[[ "$RELEASE_TAG" =~ ^v(0|[1-9][0-9]*)');
  const deleteCheckoutTag = workflow.indexOf('git tag -d "$RELEASE_TAG"');
  const fetchAnnotatedTag = workflow.indexOf('refs/tags/$RELEASE_TAG:refs/tags/$RELEASE_TAG');
  const validateAnnotatedTag = workflow.indexOf('git cat-file -t "refs/tags/$RELEASE_TAG"');
  const fetchMain = workflow.indexOf("refs/heads/main:refs/remotes/origin/main");
  const ancestry = workflow.indexOf("merge-base --is-ancestor");
  assert(deriveTag >= 0 && deriveTag < validateTagInput);
  assert(validateTagInput < deleteCheckoutTag);
  assert(deleteCheckoutTag < fetchAnnotatedTag && fetchAnnotatedTag < validateAnnotatedTag);
  assert(validateAnnotatedTag < fetchMain && fetchMain < ancestry);
  assert(workflow.includes('tag_commit="$(git rev-parse "refs/tags/$RELEASE_TAG^{commit}")"'));
  assert(workflow.includes('version="$(node --experimental-strip-types tools/release/release.ts check-tag "$RELEASE_TAG")"'));
  assert(workflow.includes('ref: ${{ needs.prepare.outputs.tag_commit }}'));
  assert(workflow.includes("unexpected draft release assets"));
  assert(workflow.includes("actions/download-artifact@v4"));
  assert(workflow.includes("workflow_dispatch:"));
  assert(!workflow.includes("inputs:"));
  assert(!workflow.includes("${{ inputs."));
  assert(workflow.includes("group: release-publication"));
  assert(!workflow.includes("release-manifest"));
  assert(!workflow.includes("python"));
  assert(!workflow.includes("gh release edit"));
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
  const setupNode = workflow.indexOf("actions/setup-node@v4");
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

test("public smoke tests use the published installer and released version", async () => {
  const workflow = await text(".github/workflows/release-public-smoke.yml");
  assert(workflow.includes("types: [published]"));
  assert(workflow.includes("https://tapid.dev/install.sh"));
  assert(workflow.includes("https://tapid.dev/install.ps1"));
  assert(workflow.includes("github.event.release.tag_name"));
  assert(workflow.includes("--version"));
  assert(workflow.includes("Install latest release through discovery"));
  assert(workflow.includes("shell: powershell"));
  assert(workflow.includes('test "$actual" = "tapid ${RELEASE_TAG#v}"'));
});

test("installers use checksums without embedded release signing", async () => {
  for (const path of ["scripts/install.sh", "scripts/install.ps1"]) {
    const installer = await text(path);
    const checksum = installer.indexOf("SHA256SUMS");
    const archiveDownload = path.endsWith(".sh")
      ? installer.indexOf('"$base/$archive" -o')
      : installer.indexOf('Save-BoundedHttpsFile "$base/$archive"');
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
    const env = { ...process.env, PATH: `${fakeBin}:${process.env.PATH}`, TAPID_TEST_FIXTURE: fixture };
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
  assert(workflow.includes("actions/setup-node@v4"));
  assert(workflow.includes("node --experimental-strip-types --test tools/check_architecture_test.ts tools/release/release_test.ts tools/release/publish_test.ts"));
});
