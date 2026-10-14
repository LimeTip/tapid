import { strictEqual as equal, rejects, match, ok } from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, mkdir, mkdtemp, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { arch, env as processEnv, platform } from "node:process";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import { renderInstallers } from "./bootstrap.ts";
import { promisify } from "node:util";

const run = promisify(execFile);
const execFileAsync = promisify(execFile);
const root = fileURLToPath(new URL("../../", import.meta.url));
const version = "1.2.3";
const target = platform === "darwin"
  ? (arch === "arm64" ? "aarch64-apple-darwin" : "x86_64-apple-darwin")
  : (arch === "arm64" ? "aarch64-unknown-linux-gnu" : "x86_64-unknown-linux-gnu");
const archive = `tapid-${version}-${target}.tar.gz`;

async function fixture() {
  const directory = await mkdtemp(join(tmpdir(), "tapid-record-installer-"));
  const payload = join(directory, "payload");
  const bin = join(directory, "bin");
  const install = join(directory, "installed");
  await mkdir(payload);
  await mkdir(bin);
  await mkdir(install);
  const fixtureHome = join(directory, "fixture-home");
  await mkdir(fixtureHome);
  await writeFile(join(payload, "tapid"), "#!/bin/sh\nif [ \"$1\" = __verify-release-record ]; then exit \"${FIXTURE_SIGNATURE_STATUS:-0}\"; fi\nif [ \"$1\" = __prepare-release-install ]; then exit \"${FIXTURE_INSTALL_POLICY_STATUS:-0}\"; fi\nprintf 'tapid 1.2.3\\n'\n");
  await chmod(join(payload, "tapid"), 0o755);
  await run("tar", ["-czf", join(directory, archive), "-C", payload, "tapid"]);
  const bytes = await readFile(join(directory, archive));
  const hash = createHash("sha256").update(bytes).digest("hex");
  const record = (url = `https://gitlab.example/releases/${archive}`) =>
    `tapid-release-v1\t${version}\n${target}\t${archive}\t${bytes.length}\t${hash}\t${url}\n`;
  await writeFile(join(directory, "record.tsv"), record());
  await writeFile(join(directory, "release.tsv.sig"), "fixture-signature\n");
  await writeFile(join(directory, "SHA256SUMS"), `${hash}  ${archive}\n`);
  await writeFile(join(bin, "curl"), `#!/bin/sh
set -eu
out=''
url=''
while [ "$#" -gt 0 ]; do
  case "$1" in
    -o) out="$2"; shift 2 ;;
    https://*) url="$1"; shift ;;
    --max-filesize|--proto|--proto-redir) shift 2 ;;
    *) shift ;;
  esac
done
printf '%s\\n' "$url" >> "$INSTALLER_FIXTURE/requests"
case "$url" in
  *.tsv.sig) cp "$INSTALLER_FIXTURE/release.tsv.sig" "$out" ;;
  *.tsv) cp "$INSTALLER_FIXTURE/record.tsv" "$out" ;;
  *) cp "$INSTALLER_FIXTURE/$(basename "$url")" "$out" ;;
esac
`);
  await chmod(join(bin, "curl"), 0o755);
  // Every runtime command fails if the installer tries to invoke one.
  for (const name of ["python3", "python", "node", "bun", "deno", "cargo"]) {
    await writeFile(join(bin, name), "#!/bin/sh\necho unexpected-runtime >&2\nexit 99\n");
    await chmod(join(bin, name), 0o755);
  }
  const bootstrapDir = join(directory, "bootstrap-assets");
  await mkdir(bootstrapDir);
  for (const bootstrapTarget of ["aarch64-apple-darwin", "x86_64-apple-darwin", "aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu", "aarch64-pc-windows-msvc", "x86_64-pc-windows-msvc"]) {
    const name = `tapid-${version}-${bootstrapTarget}.tar.gz`;
    await writeFile(join(bootstrapDir, name), bytes);
    await writeFile(join(directory, name), bytes);
  }
  await renderInstallers(bootstrapDir, version, "https://bootstrap.example/releases/v1.2.3");
  const installer = join(bootstrapDir, "install.sh");
  const env: Record<string, string | undefined> = { ...processEnv, HOME: fixtureHome, SHELL: "/bin/sh", PATH: `${bin}:${processEnv.PATH}`, INSTALLER_FIXTURE: directory };
  for (const name of ["TAPID_REPO", "TAPID_RELEASE_BASE_URL", "TAPID_RELEASE_DISCOVERY_URL", "TAPID_RELEASE_RECORD_URL"]) delete env[name];
  return { directory, install, bytes, hash, record, env, installer,
    installShell: (args: string[] = [], additions: Record<string, string> = {}) =>
      run("/bin/sh", [installer, "--install-dir", install, ...args], { env: { ...env, ...additions } }),
    defaultInstallShell: (home: string, additions: Record<string, string> = {}) =>
      run("/bin/sh", [installer], { env: { ...env, HOME: home, SHELL: "/bin/zsh", ...additions } }),
    requests: async () => (await readFile(join(directory, "requests"), "utf8"))
      .split("\n")
      .filter((request: string) => request && !request.endsWith(".sig") && !request.startsWith("https://bootstrap.example/"))
      .join("\n") + "\n",
    cleanup: () => rm(directory, { recursive: true, force: true }),
  };
}

test("Unix installer follows owned discovery and provider-neutral artifact URLs", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  try {
    await f.installShell();
    equal((await run(join(f.install, "tapid"), ["--version"])).stdout.trim(), "tapid 1.2.3");
    equal(await f.requests(), `https://tapid.dev/releases/v1/latest.tsv\nhttps://gitlab.example/releases/${archive}\n`);
    await writeFile(join(f.directory, "record.tsv"), f.record(`https://downloads.example.org/files/${archive}`));
    await f.installShell();
    ok((await f.requests()).endsWith(`https://downloads.example.org/files/${archive}\n`));
  } finally { await f.cleanup(); }
});

test("Unix single-file installer authenticates its native bootstrap before execution", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  const singleFileDir = join(f.directory, "single-file");
  const installer = join(singleFileDir, "install.sh");
  try {
    await mkdir(singleFileDir);
    await writeFile(installer, await readFile(f.installer));
    await run("/bin/sh", [installer, "--install-dir", f.install], { env: f.env });
    const requests = await readFile(join(f.directory, "requests"), "utf8");
    ok(requests.includes(`https://bootstrap.example/releases/v1.2.3/${archive}`));
    const before = await readFile(join(f.install, "tapid"));
    await writeFile(join(f.directory, "payload", "tapid"), '#!/bin/sh\ntouch "$INSTALLER_FIXTURE/executed-untrusted"\n');
    await run("tar", ["-czf", join(f.directory, archive), "-C", join(f.directory, "payload"), "tapid"]);
    await rejects(() => run("/bin/sh", [installer, "--install-dir", f.install], { env: f.env }),
      (error: any) => error.stderr.includes("bootstrap archive checksum mismatch"));
    await rejects(() => readFile(join(f.directory, "executed-untrusted")), { code: "ENOENT" });
    equal((await readFile(join(f.install, "tapid"))).equals(before), true);
  } finally { await f.cleanup(); }
});

test("Unix installer configures the default macOS zsh profile", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  const home = join(f.directory, "home");
  const profile = join(home, ".zprofile");
  const pathInstall = join(home, ".local", "bin");
  try {
    await mkdir(home);
    await f.defaultInstallShell(home);
    equal((await run(join(pathInstall, "tapid"), ["--version"])).stdout.trim(), "tapid 1.2.3");
    equal(await readFile(profile, "utf8"), "# tapid-path-managed-v1\nexport PATH=\"$HOME/.local/bin:$PATH\"\n# end tapid-path-managed-v1\n");
  } finally { await f.cleanup(); }
});

test("Unix installer accepts an existing unmarked default PATH export", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  const home = join(f.directory, "home");
  const profile = join(home, ".zprofile");
  const pathInstall = join(home, ".local", "bin");
  const existing = "export PATH=\"$HOME/.local/bin:$PATH\"\n";
  try {
    await mkdir(home);
    await writeFile(profile, existing);
    await f.defaultInstallShell(home, { PATH: `${pathInstall}:${f.env.PATH}` });
    equal((await run(join(pathInstall, "tapid"), ["--version"])).stdout.trim(), "tapid 1.2.3");
    equal(await readFile(profile, "utf8"), existing);
  } finally { await f.cleanup(); }
});

test("Unix installer validates the default zsh profile before replacing an installation", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  const home = join(f.directory, "home");
  const profile = join(home, ".zprofile");
  const pathInstall = join(home, ".local", "bin");
  const existing = "existing tapid installation\n";
  try {
    await mkdir(pathInstall, { recursive: true });
    await writeFile(join(pathInstall, "tapid"), existing);
    await writeFile(join(f.directory, "foreign-profile"), "export PATH=\"$PATH\"\n");
    await symlink(join(f.directory, "foreign-profile"), profile);
    await rejects(() => f.defaultInstallShell(home, { PATH: `${pathInstall}:${f.env.PATH}` }));
    equal(await readFile(join(pathInstall, "tapid"), "utf8"), existing);
  } finally { await f.cleanup(); }
});

test("Unix uninstaller refuses PATH removal without an owned install marker", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  const home = join(f.directory, "home");
  const profile = join(home, ".profile");
  const pathInstall = join(home, ".local", "bin");
  const managedProfile = "# tapid-path-managed-v1\nexport PATH=\"$HOME/.local/bin:$PATH\"\n# end tapid-path-managed-v1\n";
  try {
    await mkdir(pathInstall, { recursive: true });
    await writeFile(profile, managedProfile);
    await rejects(() => run("/bin/sh", [join(root, "scripts/uninstall.sh"), "--install-dir", pathInstall], {
      env: { ...f.env, HOME: home, SHELL: "/bin/sh" },
    }));
    equal(await readFile(profile, "utf8"), managedProfile);
  } finally { await f.cleanup(); }
});

test("Unix uninstaller preserves a foreign binary and removes a marked binary", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  const installDir = join(f.directory, "install");
  const binary = join(installDir, "tapid");
  try {
    await mkdir(installDir, { recursive: true });
    await writeFile(binary, "foreign binary");
    await rejects(() => run("/bin/sh", [join(root, "scripts/uninstall.sh"), "--install-dir", installDir], { env: f.env }));
    equal(await readFile(binary, "utf8"), "foreign binary");

    await writeFile(join(installDir, ".tapid-managed"), "tapid-managed-v1\n");
    const result = await run("/bin/sh", [join(root, "scripts/uninstall.sh"), "--install-dir", installDir], { env: f.env });
    equal(result.stdout, `Removed ${binary}\n`);
    equal(await readFile(binary, "utf8").catch(() => null), null);
    equal(await readFile(join(installDir, ".tapid-managed"), "utf8").catch(() => null), null);
  } finally { await f.cleanup(); }
});

test("PowerShell uninstaller preserves a foreign binary and removes a marked binary", async (context) => {
  try {
    await execFileAsync("pwsh", ["-NoProfile", "-Command", "$null"]);
  } catch {
    context.skip("PowerShell is unavailable");
    return;
  }
  const directory = await mkdtemp(join(tmpdir(), "tapid-powershell-uninstaller-"));
  const installDir = join(directory, "install");
  const binary = join(installDir, "tapid.exe");
  const uninstaller = join(root, "scripts/uninstall.ps1");
  try {
    await mkdir(installDir);
    await writeFile(binary, "foreign binary");
    await rejects(() => execFileAsync("pwsh", ["-NoProfile", "-File", uninstaller, "-InstallDir", installDir]));
    equal(await readFile(binary, "utf8"), "foreign binary");

    await writeFile(join(installDir, ".tapid-managed"), "tapid-managed-v1\n");
    await execFileAsync("pwsh", ["-NoProfile", "-File", uninstaller, "-InstallDir", installDir]);
    equal(await readFile(binary, "utf8").catch(() => null), null);
    equal(await readFile(join(installDir, ".tapid-managed"), "utf8").catch(() => null), null);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

 test("Unix installer owns an idempotent POSIX PATH block and uninstall removes only that block", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  const home = join(f.directory, "home");
  const profile = join(home, ".profile");
  const pathInstall = join(home, ".local", "bin");
  try {
    await mkdir(home);
    await mkdir(pathInstall, { recursive: true });
    await writeFile(profile, "export PATH=\"$HOME/bin:$PATH\"\n");
    const installShell = () => run("/bin/sh", [f.installer, "--install-dir", pathInstall], { env: { ...f.env, HOME: home, SHELL: "/bin/sh" } });
    await installShell();
    const first = await readFile(profile, "utf8");
    equal(first, "export PATH=\"$HOME/bin:$PATH\"\n# tapid-path-managed-v1\nexport PATH=\"$HOME/.local/bin:$PATH\"\n# end tapid-path-managed-v1\n");
    const resolved = await run("/bin/sh", ["-c", ". \"$HOME/.profile\"; command -v tapid"], { env: { ...f.env, HOME: home, SHELL: "/bin/sh" } });
    equal(resolved.stdout, `${pathInstall}/tapid\n`);
    await installShell();
    equal(await readFile(profile, "utf8"), first);
    const uninstalled = await run("/bin/sh", [join(root, "scripts/uninstall.sh"), "--install-dir", pathInstall], {
      env: { ...f.env, HOME: home, SHELL: "/bin/sh" },
    });
    equal(uninstalled.stdout, `Removed ${join(pathInstall, "tapid")}\n`);
    equal(await readFile(profile, "utf8"), "export PATH=\"$HOME/bin:$PATH\"\n");
  } finally { await f.cleanup(); }
});

test("Unix PATH management refuses symlinked and foreign startup files", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  const home = join(f.directory, "home");
  const profile = join(home, ".profile");
  const pathInstall = join(home, ".local", "bin");
  try {
    await mkdir(home);
    await mkdir(pathInstall, { recursive: true });
    const installShell = () => run("/bin/sh", [f.installer, "--install-dir", pathInstall], { env: { ...f.env, HOME: home, SHELL: "/bin/sh" } });
    await writeFile(join(f.directory, "foreign-profile"), "export PATH=\"$PATH\"\n");
    await symlink(join(f.directory, "foreign-profile"), profile);
    await rejects(installShell);
    await rm(profile);
    await writeFile(profile, "export PATH=\"$PATH\"\n");
    await chmod(profile, 0o444);
    await rejects(installShell);
  } finally { await f.cleanup(); }
});

test("Unix installer binds explicit versions and supports a record endpoint override", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  try {
    await f.installShell(["--version", "v1.2.3"]);
    ok((await f.requests()).startsWith("https://tapid.dev/releases/v1/v1.2.3.tsv\n"));
    await f.installShell(["--version", "1.2.3"], { TAPID_RELEASE_RECORD_URL: "https://releases.example/custom.tsv" });
    ok((await f.requests()).includes("https://releases.example/custom.tsv\n"));
    await rejects(() => f.installShell(["--version", "1.2.4"]), /invalid release record/);
    await rejects(() => f.installShell(["--version", "01.2.3"]), /version must be a stable release/);
    await rejects(() => f.installShell(["--version", "18446744073709551616.2.3"]), /version must be a stable release/);
    await writeFile(join(f.directory, "record.tsv"), f.record().replace("1.2.3\n", "18446744073709551615.2.3\n").replace(`\t${archive}\t`, `\ttapid-18446744073709551615.2.3-${target}.tar.gz\t`));
    await f.installShell(["--version", "18446744073709551615.2.3"]);
  } finally { await f.cleanup(); }
});

test("Unix installer rejects malformed records without fallback or replacing an installation", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  try {
    await writeFile(join(f.install, "tapid"), "preserve installed binary");
    const valid = f.record();
    const malformed = [
      valid + valid.split("\n")[1] + "\n",
      valid.trimEnd(),
      valid.replace("1.2.3\n", "18446744073709551616.2.3\n"),
      valid.replace("1.2.3\n", "184467440737095516150.2.3\n"),
      valid.replace("tapid-release-v1", "tapid-release-v2"),
      valid.replace("1.2.3\n", "01.2.3\n"),
      valid.replace(`\t${archive}\t`, "\t../tapid.tar.gz\t"),
      valid.replace(`\t${f.bytes.length}\t`, "\t0\t"),
      valid.replace(`\t${f.bytes.length}\t`, "\t536870913\t"),
      valid.replace(f.hash, f.hash.toUpperCase()),
      ...["http://host/file", "https://user:secret@host/file", "https://host/file?x=1", "https://host/file#fragment", "https://host\\evil/file", "https://host/file bad", "https://host/file\r", "https://host/file\0bad", "https://host/file\x7f"].map(url => f.record(url)),
      valid + "x".repeat(262144),
    ];
    for (const record of malformed) {
      await writeFile(join(f.directory, "record.tsv"), record);
      await rejects(() => f.installShell());
      equal(await readFile(join(f.install, "tapid"), "utf8"), "preserve installed binary");
    }
    equal(
      await f.requests(),
      "https://tapid.dev/releases/v1/latest.tsv\n".repeat(malformed.length),
    );
  } finally { await f.cleanup(); }
});

test("Unix installer rejects record size and digest mismatches", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  try {
    await writeFile(join(f.install, "tapid"), "preserve installed binary");
    await writeFile(join(f.directory, "record.tsv"), f.record().replace(f.hash, "0".repeat(64)));
    await rejects(() => f.installShell(), /checksum verification failed/);
    await writeFile(join(f.directory, "record.tsv"), f.record().replace(`\t${f.bytes.length}\t`, `\t${f.bytes.length + 1}\t`));
    await rejects(() => f.installShell(), /size does not match release record/);
    equal(await readFile(join(f.install, "tapid"), "utf8"), "preserve installed binary");
  } finally { await f.cleanup(); }
});

test("Unix installer limits historical compatibility and keeps legacy opt-in", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  try {
    const oldArchive = archive.replace("1.2.3", "0.0.10");
    await writeFile(join(f.directory, oldArchive), f.bytes);
    await writeFile(join(f.directory, "SHA256SUMS"), `${f.hash}  ${oldArchive}\n`);
    await f.installShell(["--version", "0.0.10"]);
    equal(await f.requests(), `https://github.com/LimeTip/tapid/releases/download/v0.0.10/SHA256SUMS\nhttps://github.com/LimeTip/tapid/releases/download/v0.0.10/${oldArchive}\n`);
    await writeFile(join(f.directory, "SHA256SUMS"), `${f.hash}  ${archive}\n`);
    await f.installShell(["--version", "1.2.3", "--repo", "Example/tapid"]);
    ok((await f.requests()).includes("https://github.com/Example/tapid/releases/download/v1.2.3/SHA256SUMS"));
    await rejects(() => f.installShell(["--version", "0.0.10"], { TAPID_RELEASE_RECORD_URL: "https://releases.example/custom.tsv" }), /invalid release record/);
    ok((await f.requests()).endsWith("https://releases.example/custom.tsv\n"));
    await rejects(() => f.installShell(["--repo", "Example/tapid"], { TAPID_RELEASE_RECORD_URL: "https://releases.example/custom.tsv" }), /cannot be combined/);
  } finally { await f.cleanup(); }
});

test("PowerShell record parser accepts provider migration and rejects malformed records", async (context) => {
  try { await run("pwsh", ["-NoProfile", "-Command", "$PSVersionTable.PSVersion.ToString()"]); }
  catch { context.skip("PowerShell is unavailable"); return; }
  const directory = await mkdtemp(join(tmpdir(), "tapid-ps-record-"));
  try {
    const source = await readFile(join(root, "scripts/install.ps1"), "utf8");
    const functions = source.slice(source.indexOf("function Test-ReleaseVersion"), source.indexOf("function Test-AbsolutePath"));
    const record = `tapid-release-v1\t1.2.3\nx86_64-pc-windows-msvc\ttapid-1.2.3-x86_64-pc-windows-msvc.tar.gz\t123\t${"a".repeat(64)}\thttps://gitlab.example/files/tapid.tar.gz\n`;
    const cases = [
      { source: record, version: "latest", valid: true },
      { source: record.replaceAll("1.2.3", "18446744073709551615.2.3"), version: "18446744073709551615.2.3", valid: true },
      { source: record.replace("gitlab.example", "downloads.example.org"), version: "1.2.3", valid: true },
      { source: record, version: "1.2.4", valid: false },
      ...[record.trimEnd(), "\ufeff" + record, record.replace("1.2.3\n", "18446744073709551616.2.3\n"), record + record.split("\n")[1] + "\n", record.replace("\t123\t", "\t0\t"), record.replace("\t123\t", "\t536870913\t"), record.replace("\t123\t", "\t01\t"), record.replace("tapid-1.2.3-", "../tapid-1.2.3-"), record.replace("a".repeat(64), "A".repeat(64)), record.replace("1.2.3\n", "01.2.3\n"), record + "x".repeat(262144), ""].map(source => ({ source, version: "latest", valid: false })),
      ...["http://host/file", "https://user:secret@host/file", "https://host/file?x=1", "https://host/file#fragment", "https://host\\evil/file", "https://host/file bad", "https://host/file\r", "https://host/file\0bad", "https://host/file\x7f"].map(url => ({ source: record.replace("https://gitlab.example/files/tapid.tar.gz", url), version: "latest", valid: false })),
    ];
    await writeFile(join(directory, "cases.json"), JSON.stringify(cases));
    await writeFile(join(directory, "test.ps1"), `$ErrorActionPreference = 'Stop'
$MAX_RECORD_BYTES = 256KB
$MAX_ARCHIVE_BYTES = 512MB
function Fail([string]$Message) { throw $Message }
${functions}
$cases = Get-Content -Raw (Join-Path $PSScriptRoot 'cases.json') | ConvertFrom-Json
foreach ($case in $cases) {
    $path = Join-Path $PSScriptRoot 'record.tsv'
    [IO.File]::WriteAllText($path, $case.source)
    $accepted = $false
    try {
        $record = Read-ReleaseRecord $path $case.version 'x86_64-pc-windows-msvc'
        $accepted = $true
    } catch { }
    if ($accepted -ne $case.valid) { throw "Unexpected parser result for $($case.source.Substring(0, [Math]::Min(100, $case.source.Length)))" }
}
Write-Output "Validated $($cases.Count) cases"
`);
    const result = await run("pwsh", ["-NoProfile", "-File", join(directory, "test.ps1")]);
    match(result.stdout, /Validated \d+ cases/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});


test("Unix installer rejects a native signature failure before replacing an installation", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  try {
    await f.installShell();
    const before = await readFile(join(f.install, "tapid"));
    await rejects(() => f.installShell([], { FIXTURE_SIGNATURE_STATUS: "1" }),
      (error: any) => error.stderr.includes("release record signature verification failed"));
    equal((await readFile(join(f.install, "tapid"))).equals(before), true);
  } finally { await f.cleanup(); }
});

test("Unix installer requires native rollback policy before destination replacement", { skip: platform === "win32" }, async () => {
  const f = await fixture();
  try {
    await f.installShell();
    const before = await readFile(join(f.install, "tapid"));
    await rejects(() => f.installShell([], { FIXTURE_INSTALL_POLICY_STATUS: "1" }),
      (error: any) => error.stderr.includes("release policy rejected"));
    ok((await readFile(join(f.install, "tapid"))).equals(before));
  } finally { await f.cleanup(); }
});

test("Both signed installers invoke the pinned bootstrap rollback policy before replacing bytes", async () => {
  const sh = await readFile(join(root, "scripts/install.sh"), "utf8");
  const ps = await readFile(join(root, "scripts/install.ps1"), "utf8");
  for (const [source, replacement] of [[sh, 'mv -f "$STAGED_BINARY" "$INSTALL_DIR/tapid"'], [ps, 'Move-Item -LiteralPath $staged -Destination $destination -Force']]) {
    const policy = source.indexOf("__prepare-release-install");
    ok(policy >= 0);
    ok(policy < source.lastIndexOf(replacement));
  }
});

test("PowerShell installer stops a rejected native policy before replacing destination or marker", async (context) => {
  try { await run("pwsh", ["-NoProfile", "-Command", "$null"]); }
  catch { context.skip("PowerShell is unavailable"); return; }
  const directory = await mkdtemp(join(tmpdir(), "tapid-ps-policy-"));
  try {
    const source = await readFile(join(root, "scripts/install.ps1"), "utf8");
    const start = source.indexOf("    if (-not $legacyRelease) {");
    const end = source.indexOf("    try {\n        Configure-UserPath", start);
    ok(start >= 0 && end > start);
    const block = source.slice(start, end);
    await writeFile(join(directory, "reject.ps1"), "exit 17\n");
    await writeFile(join(directory, "test.ps1"), `$ErrorActionPreference = 'Stop'
function Fail([string]$Message) { throw $Message }
$InstallDir = $PSScriptRoot
$destination = Join-Path $PSScriptRoot 'tapid.exe'
$extracted = Join-Path $PSScriptRoot 'selected.exe'
$staged = Join-Path $PSScriptRoot 'staged.exe'
$stagedMarker = Join-Path $PSScriptRoot 'staged-marker'
$marker = Join-Path $PSScriptRoot '.tapid-managed'
$bootstrap = Join-Path $PSScriptRoot 'reject.ps1'
$Version = '1.2.3'
$BootstrapVersion = '1.0.0'
$archivePath = Join-Path $PSScriptRoot 'release.tar.gz'
$legacyRelease = $false
[IO.File]::WriteAllText($destination, 'installed bytes')
[IO.File]::WriteAllText($marker, 'original marker')
[IO.File]::WriteAllText($extracted, 'replacement bytes')
$rejected = $false
try {
${block}
} catch {
    if ($_.Exception.Message -notlike '*installer release policy rejected*') { throw }
    $rejected = $true
}
if (-not $rejected) { throw 'native policy failure was accepted' }
if ([IO.File]::ReadAllText($destination) -cne 'installed bytes') { throw 'destination changed' }
if ([IO.File]::ReadAllText($marker) -cne 'original marker') { throw 'marker changed' }
if (Test-Path $staged) { throw 'replacement was staged despite policy rejection' }
$global:LASTEXITCODE = 0
Write-Output 'Policy rejection preserved installation'
`);
    const result = await run("pwsh", ["-NoProfile", "-File", join(directory, "test.ps1")]);
    match(result.stdout, /Policy rejection preserved installation/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});
