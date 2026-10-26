import { strictEqual as equal, rejects, match, ok } from "node:assert/strict";
import { execFile } from "node:child_process";
import { createHash } from "node:crypto";
import { access, chmod, mkdir, mkdtemp, readFile, realpath, rm, symlink, writeFile } from "node:fs/promises";
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

const shell = platform === "win32"
  ? await run("where.exe", ["sh"]).then(result => result.stdout.trim().split(/\r?\n/)[0], () => undefined)
  : "/bin/sh";
const shellOptions = { skip: shell ? false : "POSIX shell is unavailable" };
/** Convert native fixture paths for the POSIX shell used to execute installer artifacts. */
function shellPath(path: string): string {
  return platform === "win32" ? path.replaceAll("\\", "/").replace(/^([A-Za-z]):/, (_, drive) => "/" + drive.toLowerCase()) : path;
}

/** Build an isolated release fixture with generated installers and local download/verifier stubs. */
async function fixture() {
  if (!shell) throw new Error("POSIX shell is unavailable");
  const directory = await mkdtemp(join(await realpath(tmpdir()), "tapid-record-installer-"));
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
  if (platform === "win32") {
    await writeFile(join(bin, "uname"), `#!/bin/sh\ncase "$1" in -s) echo Linux ;; -m) echo ${arch === "arm64" ? "aarch64" : "x86_64"} ;; esac\n`);
    await chmod(join(bin, "uname"), 0o755);
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
  const shellSearchPath = platform === "win32" ? `${bin};${processEnv.PATH ?? processEnv.Path}` : `${bin}:${processEnv.PATH}`;
  const env: Record<string, string | undefined> = { ...processEnv, HOME: shellPath(fixtureHome), SHELL: "/bin/sh", PATH: shellSearchPath, INSTALLER_FIXTURE: shellPath(directory) };
  for (const name of Object.keys(env)) if (name !== "PATH" && name.toUpperCase() === "PATH") delete env[name];
  for (const name of ["TAPID_REPO", "TAPID_RELEASE_BASE_URL", "TAPID_RELEASE_DISCOVERY_URL", "TAPID_RELEASE_RECORD_URL"]) delete env[name];
  return { directory, install, bytes, hash, record, env, installer,
    installShell: (args: string[] = [], additions: Record<string, string> = {}) =>
      run(shell, [shellPath(installer), "--install-dir", shellPath(install), ...args], { env: { ...env, ...additions } }),
    defaultInstallShell: (home: string, additions: Record<string, string> = {}) =>
      run(shell, ["-c", 'PATH="$INSTALLER_FIXTURE/bin:$PATH"; export PATH; exec /bin/sh "$1" --configure-path', "installer", shellPath(installer)], { env: { ...env, HOME: shellPath(home), SHELL: "/bin/zsh", ...additions } }),
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

test("Unix installer requires consent before changing startup files", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "consent-home");
  try {
    await mkdir(home);
    const profile = join(home, ".profile");
    await writeFile(profile, "# user configuration\n");
    const result = await f.installShell(["--install-dir", shellPath(join(home, ".local", "bin")), "--shell", "posix"], { HOME: shellPath(home) });
    equal(await readFile(profile, "utf8"), "# user configuration\n");
    match(result.stdout, /Persistent PATH was not changed/);
    match(result.stdout, /export PATH=/);
  } finally { await f.cleanup(); }
});

test("Unix activation guidance quotes custom paths and preserves empty PATH segments", shellOptions, async () => {
  const f = await fixture();
  const install = join(f.directory, "bin 'quoted' $(touch injected) space");
  try {
    const result = await f.installShell(["--install-dir", shellPath(install)]);
    const command = result.stdout.split("\n").find(line => line.startsWith("To enable Tapid"))!.split("run: ")[1];
    for (const [setup, expected] of [["unset PATH", shellPath(install)], ["PATH=", `${shellPath(install)}:`], ["PATH=:/other:", `${shellPath(install)}::/other:`]]) {
      const activated = await run(shell!, ["-c", `${setup}; ${command}; printf '%s' "$PATH"`], { cwd: f.directory, env: f.env });
      equal(activated.stdout, expected);
      const repeated = await run(shell!, ["-c", `${setup}; ${command}; ${command}; printf '%s' "$PATH"`], { cwd: f.directory, env: f.env });
      equal(repeated.stdout, expected);
    }
    await rejects(() => readFile(join(f.directory, "injected")), { code: "ENOENT" });
  } finally { await f.cleanup(); }
});

test("Unix installer refuses directories that cannot be represented in PATH before downloading", shellOptions, async () => {
  const f = await fixture();
  try {
    for (const suffix of ["colon:path", "line\nbreak", "carriage\rreturn"]) {
      await rejects(() => f.installShell(["--install-dir", shellPath(join(f.directory, suffix))]), /install directory.*PATH/);
    }
    await rejects(() => readFile(join(f.directory, "requests")), { code: "ENOENT" });
  } finally { await f.cleanup(); }
});

test("Unix auto selection leaves startup files unchanged for unknown or unset preferences", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "unknown-shell-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# untouched\n");
    for (const preference of ["", "/bin/unknown-shell"]) {
      const result = await f.installShell(["--install-dir", shellPath(join(home, ".local", "bin")), "--configure-path"], { HOME: shellPath(home), SHELL: preference });
      match(result.stdout, /Persistent PATH was not changed/);
      equal(await readFile(join(home, ".profile"), "utf8"), "# untouched\n");
    }
  } finally { await f.cleanup(); }
});

test("Unix uninstall preserves edited managed blocks and retains the installation for retry", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "edited-block-home");
  const install = join(home, ".local", "bin");
  try {
    await mkdir(home);
    await f.defaultInstallShell(home, { SHELL: "/bin/sh" });
    const edited = '# tapid-path-managed-v1\nexport PATH="$HOME/.local/bin:$PATH"\n# user edit\n# end tapid-path-managed-v1\n';
    await writeFile(join(home, ".profile"), edited);
    await rejects(() => run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } }), /refusing unsafe/);
    equal(await readFile(join(home, ".profile"), "utf8"), edited);
    equal(await readFile(join(install, ".tapid-managed"), "utf8"), "tapid-managed-v1\n");
    equal((await run(join(install, "tapid"), ["--version"])).stdout.trim(), "tapid 1.2.3");
  } finally { await f.cleanup(); }
});

test("explicit Bash selection overrides a misleading SHELL", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "bash-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".bash_profile"), "# bash login\n");
    await writeFile(join(home, ".zprofile"), "# zsh login\n");
    await f.installShell(["--install-dir", shellPath(join(home, ".local", "bin")), "--shell", "bash", "--configure-path"], { HOME: shellPath(home), SHELL: "/bin/zsh" });
    match(await readFile(join(home, ".bash_profile"), "utf8"), /tapid-path-managed-v1/);
    equal(await readFile(join(home, ".zprofile"), "utf8"), "# zsh login\n");
  } finally { await f.cleanup(); }
});

test("Unix shell selection rejects invalid and unavailable preferences before installation", shellOptions, async () => {
  const f = await fixture();
  try {
    for (const args of [["--shell"], ["--shell", ""], ["--shell", "tcsh"]]) {
      await rejects(() => f.installShell(args), /--shell|fish PATH persistence/);
      await rejects(() => readFile(join(f.install, "tapid")), { code: "ENOENT" });
      await rejects(() => readFile(join(f.directory, "requests")), { code: "ENOENT" });
    }
  } finally { await f.cleanup(); }
});

test("explicit Bash selection respects login startup precedence", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "bash-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".bash_login"), "# login\n");
    await writeFile(join(home, ".profile"), "# fallback\n");
    await f.installShell(["--install-dir", shellPath(join(home, ".local", "bin")), "--shell", "bash", "--configure-path"], { HOME: shellPath(home) });
    match(await readFile(join(home, ".bash_login"), "utf8"), /tapid-path-managed-v1/);
    equal(await readFile(join(home, ".profile"), "utf8"), "# fallback\n");
    await symlink(join(home, ".bash_login"), join(home, ".bash_profile"));
    await rejects(() => f.installShell(["--install-dir", shellPath(join(home, ".local", "bin")), "--shell", "bash", "--configure-path"], { HOME: shellPath(home) }), /could not safely update/);
    await rm(join(home, ".bash_profile"));
    await run(shell!, [shellPath(join(root, "scripts", "uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home), SHELL: "/bin/bash" } });
    equal(await readFile(join(home, ".bash_login"), "utf8"), "# login\n");
    await writeFile(join(home, ".bashrc"), '# tapid-path-managed-v1\nexport PATH="$HOME/.local/bin:$PATH"\n# end tapid-path-managed-v1\n');
    await writeFile(join(home, ".local", "bin", ".tapid-managed"), "tapid-managed-v1\n");
    await run(shell!, [shellPath(join(root, "scripts", "uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home), SHELL: "/bin/bash" } });
    equal(await readFile(join(home, ".bashrc"), "utf8"), "");
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
    match(await readFile(profile, "utf8"), /^# tapid-path-managed-v1\ncase .*\n# end tapid-path-managed-v1\n$/);
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
    await f.defaultInstallShell(home);
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } });
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
    const installShell = () => run("/bin/sh", [f.installer, "--install-dir", pathInstall, "--configure-path"], { env: { ...f.env, HOME: home, SHELL: "/bin/sh" } });
    await installShell();
    const first = await readFile(profile, "utf8");
    equal(first.replace(/# tapid-path-managed-v1\n[^\n]*\n# end tapid-path-managed-v1\n/, ""), 'export PATH="$HOME/bin:$PATH"\n');
    match(first, /# tapid-path-managed-v1\ncase /);
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
    const installShell = () => run("/bin/sh", [f.installer, "--install-dir", pathInstall, "--configure-path"], { env: { ...f.env, HOME: home, SHELL: "/bin/sh" } });
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

for (const [name, original, preserved] of [
  ["no final newline", 'export PATH="$HOME/bin:$PATH"', 'export PATH="$HOME/bin:$PATH"\n'],
  ["final newline", 'export PATH="$HOME/bin:$PATH"\n', 'export PATH="$HOME/bin:$PATH"\n'],
  ["empty profile", "", ""],
  ["existing managed block", '# before\n# tapid-path-managed-v1\nexport PATH="$HOME/.local/bin:$PATH"\n# end tapid-path-managed-v1\n# after\n', '# before\n# after\n'],
]) {
  test(`Unix PATH management preserves ${name} across repeated install and uninstall`, shellOptions, async () => {
    const f = await fixture();
    const home = join(f.directory, "home");
    const profile = join(home, ".profile");
    try {
      await mkdir(home);
      await writeFile(profile, original);
      const install = () => f.defaultInstallShell(home, { SHELL: "/bin/sh" });
      await install();
      const first = await readFile(profile, "utf8");
      equal(first.replace(/# tapid-path-managed-v1\n[^\n]*\n# end tapid-path-managed-v1\n/, ""), preserved);
      equal(first.split("# tapid-path-managed-v1").length, 2);
      if (name === "existing managed block") equal(first, original);
      const resolved = await run(shell, ["-c", '. "$HOME/.profile"; command -v tapid'], { env: { ...f.env, HOME: shellPath(home) } });
      equal(resolved.stdout, shellPath(join(home, ".local", "bin", "tapid")) + "\n");
      await install();
      equal(await readFile(profile, "utf8"), first);
      await run(shell, [shellPath(join(root, "scripts/uninstall.sh")), "--install-dir", shellPath(join(home, ".local", "bin"))], { env: { ...f.env, HOME: shellPath(home) } });
      // Receipted profiles restore their exact original bytes, including EOF.
      equal(await readFile(profile, "utf8"), name === "existing managed block" ? preserved : original);
    } finally { await f.cleanup(); }
  });
}

test("Unix PATH management refuses malformed managed blocks without changing the profile", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "home");
  const profile = join(home, ".profile");
  try {
    await mkdir(home);
    for (const original of [
      '# before\n# tapid-path-managed-v1\n# user data\n',
      '# end tapid-path-managed-v1\nexport PATH="$HOME/.local/bin:$PATH"\n# tapid-path-managed-v1\n# user data\n',
      '# tapid-path-managed-v1\n# user data\n# end tapid-path-managed-v1\nexport PATH="$HOME/.local/bin:$PATH"\n',
    ]) {
      await writeFile(profile, original);
      await rejects(() => f.defaultInstallShell(home, { SHELL: "/bin/sh" }), /could not safely update/);
      equal(await readFile(profile, "utf8"), original);
    }
  } finally { await f.cleanup(); }
});

test("Unix installer verifies profile bytes after replacement before claiming PATH success", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "readback-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# original\n");
    await writeFile(join(f.directory, "bin", "mv"), '#!/bin/sh\ncase "$3" in */.profile) printf "# altered after staging\\n" > "$3"; /bin/rm -f "$2" ;; *) exec /bin/mv "$@" ;; esac\n');
    await chmod(join(f.directory, "bin", "mv"), 0o755);
    await rejects(() => f.defaultInstallShell(home, { SHELL: "/bin/sh" }), /could not safely update/);
    await rejects(() => readFile(join(home, ".local", "bin", "tapid")), { code: "ENOENT" });
  } finally { await f.cleanup(); }
});

for (const copyStatus of [0, 1]) {
  test(`Unix PATH management preserves original on invalid staged bytes with copy status ${copyStatus}`, shellOptions, async () => {
    const f = await fixture();
    const home = join(f.directory, "home");
    const profile = join(home, ".profile");
    const original = '# preserve user configuration\nexport EXISTING=1\n';
    try {
      await mkdir(home);
      await writeFile(profile, original);
      // A truncated prefix ending in LF must be rejected even if copying reports success.
      await writeFile(join(f.directory, "bin", "cat"), `#!/bin/sh\ncase "$1" in */.profile) printf '# preserve user configuration\\n'; exit ${copyStatus} ;; *) exec /bin/cat "$@" ;; esac\n`);
      await chmod(join(f.directory, "bin", "cat"), 0o755);
      await rejects(() => f.defaultInstallShell(home, { SHELL: "/bin/sh" }), /could not safely update/);
      equal(await readFile(profile, "utf8"), original);
    } finally { await f.cleanup(); }
  });
}

test("Unix receipt cleanup survives a changed SHELL and missing binary", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "receipt-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# keep\n");
    await f.defaultInstallShell(home, { SHELL: "/bin/sh" });
    await rm(join(home, ".local", "bin", "tapid"));
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home), SHELL: "/bin/zsh" } });
    equal(await readFile(join(home, ".profile"), "utf8"), "# keep\n");
    await rejects(() => access(join(home, ".local", "bin", ".tapid-path-v1")));
  } finally { await rm(f.directory, { recursive: true, force: true }); }
});

test("Unix pending receipt reconciles before and after snapshots without duplicating blocks", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "pending-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# preserved\n");
    await f.defaultInstallShell(home, { SHELL: "/bin/sh" });
    const receipt = join(home, ".local", "bin", ".tapid-path-v1");
    const expected = await readFile(join(home, ".profile"), "utf8");
    for (const snapshot of ["before", "after"]) {
      await writeFile(join(receipt, "state"), "pending\n");
      await writeFile(join(home, ".profile"), await readFile(join(receipt, snapshot)));
      await f.defaultInstallShell(home, { SHELL: "/bin/sh" });
      equal(await readFile(join(home, ".profile"), "utf8"), expected);
      equal(await readFile(join(receipt, "state"), "utf8"), "completed\n");
    }
    await writeFile(join(receipt, "state"), "pending\n");
    await writeFile(join(home, ".profile"), "# concurrent edit\n");
    await rejects(() => f.defaultInstallShell(home, { SHELL: "/bin/sh" }), /could not safely update/);
    equal(await readFile(join(home, ".profile"), "utf8"), "# concurrent edit\n");
  } finally { await f.cleanup(); }
});

test("Unix receipt parser rejects unknown fields, unsafe files, and inconsistent snapshots", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "invalid-receipt-home");
  try {
    await mkdir(home);
    await f.defaultInstallShell(home, { SHELL: "/bin/sh" });
    const receipt = join(home, ".local", "bin", ".tapid-path-v1");
    const profile = join(home, ".profile");
    const expected = await readFile(profile, "utf8");
    for (const [field, value] of [["unexpected", "data"], ["manifest", "tapid-path-v2\nunix\ninstall\n1\n"], ["state", "unknown\n"], ["target", profile + "\n"], ["payload", "execute something\n"], ["after", "# arbitrary replacement\n"], ["payload", "x".repeat(65537)], ["before", "x".repeat(8388609)]]) {
      const original = await readFile(join(receipt, field)).catch(() => undefined);
      await writeFile(join(receipt, field), value);
      await rejects(() => f.defaultInstallShell(home, { SHELL: "/bin/sh" }), /could not safely update/);
      await rejects(() => run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } }), /refusing unsafe/);
      equal(await readFile(profile, "utf8"), expected);
      if (original) await writeFile(join(receipt, field), original); else await rm(join(receipt, field));
    }
    const original = await readFile(join(receipt, "target"));
    await writeFile(join(f.directory, "foreign-target"), original);
    await rm(join(receipt, "target"));
    await symlink(join(f.directory, "foreign-target"), join(receipt, "target"));
    await rejects(() => f.defaultInstallShell(home, { SHELL: "/bin/sh" }), /could not safely update/);
    equal(await readFile(profile, "utf8"), expected);
  } finally { await f.cleanup(); }
});

test("Unix uninstall recovers a pending PATH write before executable activation", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "interrupted-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# keep\n");
    await writeFile(join(f.directory, "bin", "mv"), '#!/bin/sh\ncase "$3" in */.profile) exit 1 ;; *) exec /bin/mv "$@" ;; esac\n');
    await chmod(join(f.directory, "bin", "mv"), 0o755);
    await rejects(() => f.defaultInstallShell(home, { SHELL: "/bin/sh" }), /could not safely update/);
    await rm(join(f.directory, "bin", "mv"));
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home), SHELL: "/unknown" } });
    equal(await readFile(join(home, ".profile"), "utf8"), "# keep\n");
    await rejects(() => access(join(home, ".local", "bin", ".tapid-path-v1")));
  } finally { await f.cleanup(); }
});

test("Unix receipt cleanup preserves unrelated edits without a final newline", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "edited-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# original\n");
    await f.defaultInstallShell(home, { SHELL: "/bin/sh" });
    const profile = join(home, ".profile");
    await writeFile(profile, await readFile(profile, "utf8") + "# later user edit");
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } });
    equal(await readFile(profile, "utf8"), "# original\n# later user edit");
  } finally { await f.cleanup(); }
});

test("Unix legacy cleanup searches bounded default locations independently of SHELL", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "legacy-home");
  try {
    await mkdir(join(home, ".local", "bin"), { recursive: true });
    await writeFile(join(home, ".local", "bin", ".tapid-managed"), "tapid-managed-v1\n");
    await writeFile(join(home, ".bash_login"), '# keep\n# tapid-path-managed-v1\nexport PATH="$HOME/.local/bin:$PATH"\n# end tapid-path-managed-v1\n');
    await writeFile(join(home, ".zprofile"), "# unrelated\n");
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home), SHELL: "/unknown" } });
    equal(await readFile(join(home, ".bash_login"), "utf8"), "# keep\n");
    equal(await readFile(join(home, ".zprofile"), "utf8"), "# unrelated\n");
  } finally { await f.cleanup(); }
});

test("Unix installer preserves profile edits made while staging its receipt", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "racing-home");
  try {
    await mkdir(home);
    const profile = join(home, ".profile");
    await writeFile(profile, "# original\n");
    await writeFile(join(f.directory, "bin", "cat"), '#!/bin/sh\ncase "$1" in */.profile) if [ ! -e "$INSTALLER_FIXTURE/cat-seen" ]; then : > "$INSTALLER_FIXTURE/cat-seen"; else printf "# concurrent edit\\n" > "$1"; fi ;; esac\nexec /bin/cat "$@"\n');
    await chmod(join(f.directory, "bin", "cat"), 0o755);
    await rejects(() => f.defaultInstallShell(home, { SHELL: "/bin/sh" }), /could not safely update/);
    equal(await readFile(profile, "utf8"), "# concurrent edit\n");
  } finally { await f.cleanup(); }
});

test("Unix installer retries an interruption after profile read-back before receipt completion", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "completion-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# keep\n");
    await writeFile(join(f.directory, "bin", "mv"), '#!/bin/sh\ncase "$3" in */.tapid-path-v1/state) exit 1 ;; *) exec /bin/mv "$@" ;; esac\n');
    await chmod(join(f.directory, "bin", "mv"), 0o755);
    await rejects(() => f.defaultInstallShell(home, { SHELL: "/bin/sh" }), /could not safely update/);
    const receipt = join(home, ".local", "bin", ".tapid-path-v1");
    equal(await readFile(join(receipt, "state"), "utf8"), "pending\n");
    const expected = await readFile(join(home, ".profile"), "utf8");
    match(expected, /tapid-path-managed-v1/);
    await rm(join(f.directory, "bin", "mv"));
    await f.defaultInstallShell(home, { SHELL: "/bin/sh" });
    equal(await readFile(join(home, ".profile"), "utf8"), expected);
    equal(await readFile(join(receipt, "state"), "utf8"), "completed\n");
    equal((await run(shell!, [shellPath(join(home, ".local", "bin", "tapid")), "--version"], { env: f.env })).stdout, "tapid 1.2.3\n");
  } finally { await f.cleanup(); }
});

test("Bash shell-mode both records and cleans both startup files", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "both-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".bash_login"), "# login\n");
    await writeFile(join(home, ".bashrc"), "# interactive\n");
    const args = ["--install-dir", shellPath(join(home, ".local", "bin")), "--configure-path", "--shell", "bash", "--shell-mode", "both"];
    await f.installShell(args, { HOME: shellPath(home), SHELL: "/bin/zsh" });
    for (const name of [".bash_login", ".bashrc"]) match(await readFile(join(home, name), "utf8"), /tapid-path-managed-v1/);
    await f.installShell(args, { HOME: shellPath(home) });
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home), SHELL: "/bin/sh" } });
    equal(await readFile(join(home, ".bash_login"), "utf8"), "# login\n");
    equal(await readFile(join(home, ".bashrc"), "utf8"), "# interactive\n");
  } finally { await f.cleanup(); }
});


test("Unix one-profile receipt migrates to Bash both without adopting user configuration", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "migration-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# login without newline");
    await writeFile(join(home, ".bashrc"), "# interactive\n");
    const args = ["--install-dir", shellPath(join(home, ".local", "bin")), "--configure-path", "--shell", "bash", "--shell-mode"];
    await f.installShell([...args, "login"], { HOME: shellPath(home) });
    const login = await readFile(join(home, ".profile"), "utf8");
    await f.installShell([...args, "both"], { HOME: shellPath(home) });
    equal(await readFile(join(home, ".profile"), "utf8"), login);
    const receipt = join(home, ".local", "bin", ".tapid-path-v1");
    equal(await readFile(join(receipt, "manifest"), "utf8"), "tapid-path-v1\nunix\ninstall\n2\n");
    equal(await readFile(join(receipt, "1", "before"), "utf8"), "# login without newline");
    await f.installShell([...args, "both"], { HOME: shellPath(home) });
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home), SHELL: "/bin/fish" } });
    equal(await readFile(join(home, ".profile"), "utf8"), "# login without newline");
    equal(await readFile(join(home, ".bashrc"), "utf8"), "# interactive\n");
  } finally { await f.cleanup(); }
});


test("Unix receipt migration recovers publication and profile-write interruptions", shellOptions, async () => {
  for (const transition of ["backup", "publish", "retire", "first", "second", "uninstall-gap", "uninstall-published"]) {
    const f = await fixture();
    const home = join(f.directory, "migration-recovery-home");
    try {
      await mkdir(home);
      await writeFile(join(home, ".profile"), "# login\n");
      await writeFile(join(home, ".bashrc"), "# interactive\n");
      const install = join(home, ".local", "bin");
      const receipt = join(install, ".tapid-path-v1");
      const args = ["--install-dir", shellPath(install), "--configure-path", "--shell", "bash", "--shell-mode"];
      await f.installShell([...args, "login"], { HOME: shellPath(home) });
      // Include the original legacy payload and pending-before state.
      if (transition === "first") {
        const payload = 'export PATH="$HOME/.local/bin:$PATH"\n';
        await writeFile(join(receipt, "payload"), payload);
        await writeFile(join(receipt, "after"), "# login\n# tapid-path-managed-v1\n" + payload + "# end tapid-path-managed-v1\n");
        await writeFile(join(receipt, "state"), "pending\n");
        await writeFile(join(home, ".profile"), "# login\n");
      }
      const refusal = transition === "backup" ? 'case "$2" in */.tapid-path-previous-v1) exit 1 ;; esac'
        : ["publish", "uninstall-gap"].includes(transition) ? 'case "$1:$2" in */.tapid-path-stage.*:*/.tapid-path-v1) exit 1 ;; esac'
        : ["retire", "uninstall-published"].includes(transition) ? 'case "$1:$2" in */.tapid-path-previous-v1:*/receipt) exit 1 ;; esac'
        : 'case "$3" in */.tapid-path-v1/' + (transition === "first" ? "1" : "2") + '/state) exit 1 ;; esac';
      await writeFile(join(f.directory, "bin", "mv"), '#!/bin/sh\n' + refusal + '\nexec /bin/mv "$@"\n');
      await chmod(join(f.directory, "bin", "mv"), 0o755);
      await rejects(() => f.installShell([...args, "both"], { HOME: shellPath(home) }), /could not safely update/);
      await rm(join(f.directory, "bin", "mv"));
      if (transition === "retire") {
        const previous = join(install, ".tapid-path-previous-v1");
        await writeFile(join(previous, "unexpected"), "foreign data");
        await rejects(() => f.installShell([...args, "both"], { HOME: shellPath(home) }), /could not safely update/);
        await rejects(() => run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } }), /inconsistent PATH migration/);
        await rm(join(previous, "unexpected"));
        const target = await readFile(join(previous, "target"));
        await writeFile(join(previous, "target"), shellPath(join(home, "unowned")));
        await rejects(() => f.installShell([...args, "both"], { HOME: shellPath(home) }), /could not safely update/);
        await rejects(() => run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } }), /inconsistent PATH migration/);
        equal(await readFile(join(home, ".bashrc"), "utf8"), "# interactive\n");
        await writeFile(join(previous, "target"), target);
      }
      if (!transition.startsWith("uninstall")) {
        await f.installShell([...args, "both"], { HOME: shellPath(home) });
        for (const name of [".profile", ".bashrc"]) equal((await readFile(join(home, name), "utf8")).split("# tapid-path-managed-v1").length, 2);
      }
      await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } });
      equal(await readFile(join(home, ".profile"), "utf8"), "# login\n");
      equal(await readFile(join(home, ".bashrc"), "utf8"), "# interactive\n");
      await rejects(() => access(join(install, ".tapid-path-previous-v1")), { code: "ENOENT" });
    } finally { await f.cleanup(); }
  }
});

test("Unix receipt migration preflights conflicts and preserves unmarked equivalent exports", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "migration-conflict-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# login\n");
    const install = join(home, ".local", "bin");
    const args = ["--install-dir", shellPath(install), "--configure-path", "--shell", "bash", "--shell-mode"];
    await f.installShell([...args, "interactive"], { HOME: shellPath(home) });
    const interactive = await readFile(join(home, ".bashrc"), "utf8");
    const edited = interactive.replace("# end tapid-path-managed-v1", "# user edit\n# end tapid-path-managed-v1");
    await writeFile(join(home, ".bashrc"), edited);
    await rejects(() => f.installShell([...args, "both"], { HOME: shellPath(home) }), /could not safely update/);
    equal(await readFile(join(home, ".profile"), "utf8"), "# login\n");
    equal(await readFile(join(home, ".bashrc"), "utf8"), edited);
    await writeFile(join(home, ".bashrc"), interactive);
    await rm(join(home, ".profile"));
    await symlink(join(home, ".bashrc"), join(home, ".profile"));
    await rejects(() => f.installShell([...args, "both"], { HOME: shellPath(home) }), /could not safely update/);
    equal(await readFile(join(home, ".bashrc"), "utf8"), interactive);
    await rm(join(home, ".profile"));
    const preexisting = 'export PATH="$HOME/.local/bin:$PATH"\n';
    await writeFile(join(home, ".profile"), preexisting);
    await f.installShell([...args, "both"], { HOME: shellPath(home) });
    equal(await readFile(join(install, ".tapid-path-v1", "1", "state"), "utf8"), "preexisting\n");
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } });
    equal(await readFile(join(home, ".profile"), "utf8"), preexisting);
    equal(await readFile(join(home, ".bashrc"), "utf8"), "");
  } finally { await f.cleanup(); }
});


test("Unix migration rejects unsafe recovery receipts and preserves edits during staging", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "migration-safety-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# login\n");
    await writeFile(join(home, ".bashrc"), "# interactive\n");
    const install = join(home, ".local", "bin");
    const receipt = join(install, ".tapid-path-v1");
    const previous = join(install, ".tapid-path-previous-v1");
    const args = ["--install-dir", shellPath(install), "--configure-path", "--shell", "bash", "--shell-mode"];
    await f.installShell([...args, "login"], { HOME: shellPath(home) });
    const login = await readFile(join(home, ".profile"), "utf8");
    await symlink(receipt, previous);
    await rejects(() => f.installShell([...args, "both"], { HOME: shellPath(home) }), /could not safely update/);
    await rejects(() => run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } }), /inconsistent PATH migration/);
    equal(await readFile(join(home, ".profile"), "utf8"), login);
    await rm(previous);
    // Change the old receipt target after it was copied into the staged receipt.
    await writeFile(join(f.directory, "bin", "cp"), '#!/bin/sh\n/bin/cp "$@" || exit 1\nprintf "# concurrent edit\\n" > "$HOME/.profile"\n');
    await chmod(join(f.directory, "bin", "cp"), 0o755);
    await rejects(() => f.installShell([...args, "both"], { HOME: shellPath(home) }), /could not safely update/);
    equal(await readFile(join(home, ".profile"), "utf8"), "# concurrent edit\n");
    equal(await readFile(join(home, ".bashrc"), "utf8"), "# interactive\n");
    equal(await readFile(join(receipt, "manifest"), "utf8"), "tapid-path-v1\nunix\ninstall\n1\n");
    await rm(join(f.directory, "bin", "cp"));
  } finally { await f.cleanup(); }
});

test("Bash both preflights all targets and recovers per-profile completion", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "both-recovery-home");
  try {
    await mkdir(home);
    await writeFile(join(home, ".profile"), "# preserved\n");
    await symlink(join(home, ".profile"), join(home, ".bashrc"));
    const args = ["--install-dir", shellPath(join(home, ".local", "bin")), "--configure-path", "--shell", "bash", "--shell-mode", "both"];
    await rejects(() => f.installShell(args, { HOME: shellPath(home) }), /could not safely update/);
    equal(await readFile(join(home, ".profile"), "utf8"), "# preserved\n");
    await rm(join(home, ".bashrc"));
    await writeFile(join(f.directory, "bin", "mv"), '#!/bin/sh\ncase "$3" in */.tapid-path-v1/2/state) exit 1 ;; *) exec /bin/mv "$@" ;; esac\n');
    await chmod(join(f.directory, "bin", "mv"), 0o755);
    await rejects(() => f.installShell(args, { HOME: shellPath(home) }), /could not safely update/);
    const receipt = join(home, ".local", "bin", ".tapid-path-v1");
    equal(await readFile(join(receipt, "1", "state"), "utf8"), "completed\n");
    equal(await readFile(join(receipt, "2", "state"), "utf8"), "pending\n");
    await rm(join(f.directory, "bin", "mv"));
    await f.installShell(args, { HOME: shellPath(home) });
    for (const name of [".profile", ".bashrc"]) equal((await readFile(join(home, name), "utf8")).split("# tapid-path-managed-v1").length, 2);
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } });
    equal(await readFile(join(home, ".profile"), "utf8"), "# preserved\n");
    equal(await readFile(join(home, ".bashrc"), "utf8"), "");
  } finally { await f.cleanup(); }
});

test("Unix custom directory blocks preserve literal paths and repeated activation", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "custom-home");
  const install = join(home, "custom ' dollar$ and backslash\\ bin");
  try {
    await mkdir(home);
    await f.installShell(["--install-dir", shellPath(install), "--configure-path", "--shell", "posix"], { HOME: shellPath(home) });
    const profile = await readFile(join(home, ".profile"), "utf8");
    match(profile, /tapid-path-managed-v1/);
    const probe = await run(shell!, ["-c", '. "$HOME/.profile"; . "$HOME/.profile"; printf "%s" "$PATH"'], { env: { ...f.env, HOME: shellPath(home), PATH: "" } });
    equal(probe.stdout, shellPath(install) + ":");
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh")), "--install-dir", shellPath(install)], { env: { ...f.env, HOME: shellPath(home) } });
    equal(await readFile(join(home, ".profile"), "utf8"), "");
  } finally { await f.cleanup(); }
});

test("Unix startup modes reject invalid selections before downloading", shellOptions, async () => {
  const f = await fixture();
  try {
    for (const args of [["--shell-mode"], ["--shell", "bash", "--shell-mode", ""], ["--shell", "bash", "--shell-mode", "other"], ["--shell", "posix", "--shell-mode", "both"], ["--shell", "auto", "--shell-mode", "login"]]) {
      await rejects(() => f.installShell(args, { SHELL: "" }), /shell-mode|POSIX supports/);
    }
    await rejects(() => readFile(join(f.directory, "requests")), { code: "ENOENT" });
  } finally { await f.cleanup(); }
});

test("Zsh modes honor an absolute ZDOTDIR and reject ambiguous directories", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "zdot-home");
  const config = join(home, "zsh config");
  try {
    await mkdir(config, { recursive: true });
    const args = ["--install-dir", shellPath(join(home, ".local", "bin")), "--configure-path", "--shell", "zsh", "--shell-mode", "interactive"];
    for (const value of ["", "relative"]) await rejects(() => f.installShell(args, { HOME: shellPath(home), ZDOTDIR: value }), /ZDOTDIR/);
    await rejects(() => readFile(join(f.directory, "requests")), { code: "ENOENT" });
    await f.installShell(args, { HOME: shellPath(home), ZDOTDIR: shellPath(config) });
    match(await readFile(join(config, ".zshrc"), "utf8"), /tapid-path-managed-v1/);
    await rejects(() => readFile(join(config, ".zprofile")), { code: "ENOENT" });
    await rejects(() => readFile(join(home, ".zshrc")), { code: "ENOENT" });
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home), ZDOTDIR: "/unused", SHELL: "/bin/sh" } });
    equal(await readFile(join(config, ".zshrc"), "utf8"), "");
  } finally { await f.cleanup(); }
});

test("Native Bash, Zsh and POSIX startup modes resolve a custom installation automatically", { skip: platform === "win32" }, async (context) => {
  for (const selected of ["bash", "zsh", "posix"]) {
    await context.test(selected, async (context) => {
      const executable = await run("/bin/sh", ["-c", "command -v " + (selected === "posix" ? "sh" : selected)]).then(result => result.stdout.trim(), () => "");
      if (!executable) { equal(processEnv.TAPID_REQUIRE_NATIVE_SHELLS, undefined, "required native " + selected + " is unavailable"); context.skip(selected + " is unavailable"); return; }
      const f = await fixture();
      const home = join(f.directory, selected + "-native-home");
      const install = join(home, "custom bin");
      try {
        await mkdir(home);
        const args = ["--install-dir", shellPath(install), "--configure-path", "--shell", selected, "--shell-mode", selected === "posix" ? "login" : "both"];
        const modes = selected === "posix" ? ["-l"] : ["-l", "-i"];
        await f.installShell(args, { HOME: shellPath(home), ZDOTDIR: shellPath(home) });
        const probeEnv = { ...f.env, HOME: shellPath(home), ZDOTDIR: shellPath(home), PATH: "/usr/bin:/bin", BASH_ENV: "", ENV: "" };
        for (const mode of modes) {
          const probe = await run(executable, [mode, "-c", "command -v tapid"], { env: probeEnv });
          equal(probe.stdout.trim(), shellPath(join(install, "tapid")));
        }
        await run(shell!, [shellPath(join(root, "scripts/uninstall.sh")), "--install-dir", shellPath(install)], { env: { ...f.env, HOME: shellPath(home) } });
        for (const mode of modes) {
          const probe = await run(executable, [mode, "-c", 'printf "%s" "$PATH"'], { env: probeEnv });
          ok(!probe.stdout.split(":").includes(shellPath(install)));
        }
      } finally { await f.cleanup(); }
    });
  }
});

test("Bash both recovers an unwritten second target and rejects malformed records", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "both-unwritten-home");
  try {
    await mkdir(home);
    const args = ["--install-dir", shellPath(join(home, ".local", "bin")), "--configure-path", "--shell", "bash", "--shell-mode", "both"];
    await writeFile(join(f.directory, "bin", "mv"), '#!/bin/sh\ncase "$3" in */.bashrc) exit 1 ;; *) exec /bin/mv "$@" ;; esac\n');
    await chmod(join(f.directory, "bin", "mv"), 0o755);
    await rejects(() => f.installShell(args, { HOME: shellPath(home) }), /could not safely update/);
    const receipt = join(home, ".local", "bin", ".tapid-path-v1");
    equal(await readFile(join(receipt, "1", "state"), "utf8"), "completed\n");
    equal(await readFile(join(receipt, "2", "state"), "utf8"), "pending\n");
    await rejects(() => readFile(join(home, ".bashrc")), { code: "ENOENT" });
    await rm(join(f.directory, "bin", "mv"));
    const first = await readFile(join(home, ".profile"), "utf8");
    await writeFile(join(home, ".bashrc"), "# concurrent edit\n");
    await rejects(() => f.installShell(args, { HOME: shellPath(home) }), /could not safely update/);
    equal(await readFile(join(home, ".profile"), "utf8"), first);
    equal(await readFile(join(home, ".bashrc"), "utf8"), "# concurrent edit\n");
    await rm(join(home, ".bashrc"));
    await f.installShell(args, { HOME: shellPath(home) });
    await writeFile(join(receipt, "2", "unexpected"), "foreign field\n");
    await rejects(() => run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } }), /refusing unsafe/);
    equal(await readFile(join(home, ".profile"), "utf8"), first);
    await rm(join(receipt, "2", "unexpected"));
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh"))], { env: { ...f.env, HOME: shellPath(home) } });
    equal(await readFile(join(home, ".profile"), "utf8"), "");
    equal(await readFile(join(home, ".bashrc"), "utf8"), "");
  } finally { await f.cleanup(); }
});

test("Fish selection rejects missing capabilities before downloading", shellOptions, async () => {
  const f = await fixture();
  try {
    await writeFile(join(f.directory, "bin", "fish"), '#!/bin/sh\nexit 1\n');
    await chmod(join(f.directory, "bin", "fish"), 0o755);
    await rejects(() => f.installShell(["--shell", "fish", "--configure-path"]), /fish_add_path/);
    await rejects(() => readFile(join(f.directory, "requests")), { code: "ENOENT" });
  } finally { await f.cleanup(); }
});

test("Fish startup modes resolve literal custom paths and clean recorded XDG locations", { skip: platform === "win32" }, async (context) => {
  const executable = await run("/bin/sh", ["-c", "command -v fish"]).then(result => result.stdout.trim(), () => "");
  if (!executable) { equal(processEnv.TAPID_REQUIRE_FISH, undefined, "required native fish is unavailable"); context.skip("fish is unavailable"); return; }
  for (const mode of ["login", "interactive", "both"]) {
    await context.test(mode, async () => {
      const f = await fixture();
      const home = join(f.directory, "fish-home");
      const config = join(home, "xdg config");
      const install = join(home, "custom ' dollar$ backslash\\ bin");
      const profile = join(config, "fish", "config.fish");
      const additions = { HOME: shellPath(home), XDG_CONFIG_HOME: shellPath(config), SHELL: "/bin/bash" };
      try {
        await mkdir(join(config, "fish"), { recursive: true });
        await writeFile(profile, "# keep fish config\n");
        const args = ["--install-dir", shellPath(install), "--configure-path", "--shell", "fish", "--shell-mode", mode];
        await f.installShell(args, additions);
        const original = await readFile(profile, "utf8");
        match(original, /fish_add_path --path/);
        await f.installShell(args, additions);
        equal(await readFile(profile, "utf8"), original);
        const probeEnv = { ...f.env, ...additions, PATH: "/usr/bin:/bin" };
        for (const [flags, active] of [[["-l"], mode !== "interactive"], [["-i"], mode !== "login"], [[], false]] as const) {
          const probe = await run(executable, [...flags, "-c", "command -s tapid; or true; if set -q -U fish_user_paths; exit 9; end; true"], { env: probeEnv });
          equal(probe.stdout.trim(), active ? shellPath(join(install, "tapid")) : "");
        }
        if (mode === "both") {
          for (const value of ["", undefined]) {
            const emptyEnv = { ...probeEnv, PATH: value };
            for (const unrelated of [false, true]) {
              // Compare activation against the same process's startup state. System
              // configuration belongs to the automatic startup probes above.
              const activated = await run(executable, ["--no-config", "-i", "-c", [
                unrelated ? "set --append PATH /unrelated/startup/bin /another/startup/bin" : "",
                'printf "%s\\n" "$PATH"',
                'source "$XDG_CONFIG_HOME/fish/config.fish"',
                'source "$XDG_CONFIG_HOME/fish/config.fish"',
                'printf "%s\\n" "$PATH"',
                "if set -q -U fish_user_paths; exit 9; end; true",
              ].join("; ")], { env: emptyEnv });
              const lines = activated.stdout.split("\n");
              equal(lines.length, 3);
              const [baseline, after, end] = lines;
              equal(end, "");
              if (unrelated) ok(baseline.endsWith("/unrelated/startup/bin:/another/startup/bin"));
              equal(after, shellPath(install) + ":" + baseline);
            }
          }
        }
        await run(shell!, [shellPath(join(root, "scripts/uninstall.sh")), "--install-dir", shellPath(install)], { env: { ...f.env, HOME: shellPath(home), XDG_CONFIG_HOME: "/unused", SHELL: "/bin/sh" } });
        equal(await readFile(profile, "utf8"), "# keep fish config\n");
      } finally { await f.cleanup(); }
    });
  }
});

test("Fish no-consent guidance activates the current shell independently of startup mode", { skip: platform === "win32" }, async (context) => {
  const executable = await run("/bin/sh", ["-c", "command -v fish"]).then(result => result.stdout.trim(), () => "");
  if (!executable) { equal(processEnv.TAPID_REQUIRE_FISH, undefined, "required native fish is unavailable"); context.skip("fish is unavailable"); return; }
  const f = await fixture();
  try {
    const result = await f.installShell(["--shell", "fish", "--shell-mode", "login"], { XDG_CONFIG_HOME: join(f.directory, "untouched") });
    await rejects(() => readFile(join(f.directory, "untouched", "fish", "config.fish")), { code: "ENOENT" });
    const command = result.stdout.split("\n").find(line => line.startsWith("To enable Tapid"))!.split("run: ")[1];
    const probe = await run(executable, ["--no-config", "-c", command + "; command -s tapid"], { env: { ...f.env, PATH: "/usr/bin:/bin" } });
    equal(probe.stdout.trim(), shellPath(join(f.install, "tapid")));
  } finally { await f.cleanup(); }
});

test("Fish XDG discovery rejects unsafe targets and preserves edited owned blocks", shellOptions, async () => {
  const f = await fixture();
  const home = join(f.directory, "fish-safety-home");
  const config = join(home, ".config");
  try {
    await mkdir(home);
    // Capability boundary stub: configuration safety does not require a native fish.
    await writeFile(join(f.directory, "bin", "fish"), "#!/bin/sh\nexit 0\n");
    await chmod(join(f.directory, "bin", "fish"), 0o755);
    const args = ["--configure-path", "--shell", "fish"];
    for (const value of ["", "relative", "/line\nbreak", "/line\rbreak"]) {
      await rejects(() => f.installShell(args, { HOME: shellPath(home), XDG_CONFIG_HOME: value }), /XDG_CONFIG_HOME/);
    }
    await rejects(() => readFile(join(f.directory, "requests")), { code: "ENOENT" });
    await mkdir(config);
    await symlink(join(f.directory, "installed"), join(config, "fish"));
    await rejects(() => f.installShell(args, { HOME: shellPath(home), XDG_CONFIG_HOME: shellPath(config) }), /could not safely update/);
    await rm(join(config, "fish"));
    const environment = { ...f.env, HOME: shellPath(home) };
    delete environment.XDG_CONFIG_HOME;
    await run(shell!, [shellPath(f.installer), "--install-dir", shellPath(f.install), ...args], { env: environment });
    const profile = join(config, "fish", "config.fish");
    const original = await readFile(profile, "utf8");
    match(original, /status is-login; or status is-interactive/);
    await rejects(() => f.installShell([...args, "--shell-mode", "login"], { HOME: shellPath(home), XDG_CONFIG_HOME: shellPath(config) }), /could not safely update/);
    equal(await readFile(profile, "utf8"), original);
    await writeFile(profile, original.replace("fish_add_path", "echo user-edited"));
    await rejects(() => run(shell!, [shellPath(join(root, "scripts/uninstall.sh")), "--install-dir", shellPath(f.install)], { env: environment }), /refusing unsafe/);
    match(await readFile(profile, "utf8"), /user-edited/);
    await writeFile(profile, original);
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh")), "--install-dir", shellPath(f.install)], { env: environment });
    equal(await readFile(profile, "utf8"), "");
  } finally { await f.cleanup(); }
});

test("Fish pending receipts recover and uninstall without the binary or current fish preference", shellOptions, async () => {
  const f = await fixture();
  const config = join(f.directory, "fish-recovery-config");
  try {
    await writeFile(join(f.directory, "bin", "fish"), "#!/bin/sh\nexit 0\n");
    await chmod(join(f.directory, "bin", "fish"), 0o755);
    await writeFile(join(f.directory, "bin", "mv"), '#!/bin/sh\ncase "$3" in */.tapid-path-v1/state) exit 1 ;; *) exec /bin/mv "$@" ;; esac\n');
    await chmod(join(f.directory, "bin", "mv"), 0o755);
    const args = ["--configure-path", "--shell", "auto"];
    const additions = { SHELL: "/usr/bin/fish", XDG_CONFIG_HOME: shellPath(config) };
    await rejects(() => f.installShell(args, additions), /could not safely update/);
    const receipt = join(f.install, ".tapid-path-v1");
    equal(await readFile(join(receipt, "state"), "utf8"), "pending\n");
    const profile = join(config, "fish", "config.fish");
    const original = await readFile(profile, "utf8");
    await rm(join(f.directory, "bin", "mv"));
    await f.installShell(args, additions);
    equal(await readFile(profile, "utf8"), original);
    equal(await readFile(join(receipt, "state"), "utf8"), "completed\n");
    await rm(join(f.install, "tapid"));
    await run(shell!, [shellPath(join(root, "scripts/uninstall.sh")), "--install-dir", shellPath(f.install)], { env: { ...f.env, SHELL: "/bin/sh", XDG_CONFIG_HOME: "/unused" } });
    equal(await readFile(profile, "utf8"), "");
    await rejects(() => access(receipt), { code: "ENOENT" });
  } finally { await f.cleanup(); }
});

// Source-build fixtures exercise the generated installer without Git, Cargo or network access.
const powerShellSourceStubs = `function git { $global:LASTEXITCODE = 0 }
function cargo {
  $global:BuildCalls++
  $buildRoot = $args[-1]
  New-Item -ItemType Directory -Force (Join-Path $buildRoot 'bin') | Out-Null
  [IO.File]::WriteAllText((Join-Path $buildRoot 'bin/tapid.exe'), 'fixture')
  $global:LASTEXITCODE = 0
}`;

test("generated PowerShell installer preserves PATH without consent and prints literal activation", async (context) => {
  try { await run("pwsh", ["-NoProfile", "-Command", "$null"]); }
  catch { context.skip("PowerShell is unavailable"); return; }
  const f = await fixture();
  const directory = f.directory;
  try {
    await writeFile(join(directory, "install.ps1"), await readFile(join(directory, "bootstrap-assets/install.ps1")));
    await writeFile(join(directory, "test.ps1"), `$ErrorActionPreference = 'Stop'
${powerShellSourceStubs}
$global:BuildCalls = 0
$before = $env:Path
$userBefore = [Environment]::GetEnvironmentVariable('Path', 'User')
$install = Join-Path $PSScriptRoot 'literal '' dollar $() bin'
$output = & (Join-Path $PSScriptRoot 'install.ps1') -SourceRef fixture -InstallDir $install
if ($env:Path -cne $before) { throw 'process PATH changed without consent' }
if ([Environment]::GetEnvironmentVariable('Path', 'User') -cne $userBefore) { throw 'user PATH changed without consent' }
if (-not (Test-Path (Join-Path $install 'tapid.exe'))) { throw 'binary missing' }
$guidance = @($output | Where-Object { $_ -like '$env:Path =*' })
if ($guidance.Count -ne 1) { throw 'literal activation command missing' }
Invoke-Expression $guidance[0]
if ($env:Path.Split(';')[0] -cne $install) { throw 'activation did not preserve literal directory' }
$activated = $env:Path
Invoke-Expression $guidance[0]
if ($env:Path -cne $activated) { throw 'repeated activation duplicated the directory' }
foreach ($invalid in @($install + ';bad', $install + [char]10 + 'bad', $install + '%PATH%')) {
    $calls = $global:BuildCalls
    $refused = $false
    try { & (Join-Path $PSScriptRoot 'install.ps1') -SourceRef fixture -InstallDir $invalid -ConfigurePath | Out-Null } catch { $refused = $true }
    if (-not $refused -or $global:BuildCalls -ne $calls) { throw 'invalid PATH directory was not rejected before building' }
}
Write-Output 'No-consent preservation verified'
`);
    const result = await run("pwsh", ["-NoProfile", "-File", join(directory, "test.ps1")]);
    match(result.stdout, /No-consent preservation verified/);
  } finally { await rm(directory, { recursive: true, force: true }); }
});

test("generated PowerShell installer preserves opaque PATH tokens and compares literal entries", async (context) => {
  try { await run("pwsh", ["-NoProfile", "-Command", "$null"]); }
  catch { context.skip("PowerShell is unavailable"); return; }
  const f = await fixture();
  try {
    // Substitute only the platform persistence boundary; execute the generated installer unchanged otherwise.
    const source = await readFile(join(f.directory, "bootstrap-assets/install.ps1"), "utf8");
    await writeFile(join(f.directory, "install.ps1"), source.replaceAll("Configure-UserPath $InstallDir", "Configure-UserPath $global:FixtureDirectory").replaceAll("function Read-UserPath", "function Read-NativeUserPath").replaceAll("function Write-UserPath", "function Write-NativeUserPath"));
    await writeFile(join(f.directory, "uninstall.ps1"), (await readFile(join(root, "scripts/uninstall.ps1"), "utf8")).replaceAll("function Read-UserPath", "function Read-NativeUserPath").replaceAll("function Write-UserPath", "function Write-NativeUserPath").replaceAll("Read-PathReceipt $receipt $InstallDir", "Read-PathReceipt $receipt $global:FixtureDirectory"));
    await writeFile(join(f.directory, "test.ps1"), `$ErrorActionPreference = 'Stop'
function Read-UserPath { return $global:FixturePath.PSObject.Copy() }
function Write-UserPath($Snapshot) { if (-not $global:FixtureSuppressWrite) { $global:FixturePath = $Snapshot.PSObject.Copy() }; $global:FixtureWrites++ }
${powerShellSourceStubs}
$install = Join-Path $PSScriptRoot 'installed'
$cases = @(
  @{ Before = 'relative;; %UNKNOWN%;C:\\other;'; After = 'relative;; %UNKNOWN%;C:\\other;;C:\\Tapid' },
  @{ Before = ''; After = ';C:\\Tapid' },
  @{ Before = $null; After = 'C:\\Tapid' },
  @{ Before = 'c:/tapid/;;relative'; After = 'c:/tapid/;;relative' },
  @{ Before = 'C:\\TAPID\\;other'; After = 'C:\\TAPID\\;other' },
  @{ Before = '%TAPID_EQUIVALENT%;other'; After = '%TAPID_EQUIVALENT%;other' },
  @{ Before = 'C:Tapid;C:\\bad?; C:\\Tapid'; After = 'C:Tapid;C:\\bad?; C:\\Tapid;C:\\Tapid' },
  @{ Directory = 'C:\\'; Before = 'c:/;relative'; After = 'c:/;relative' },
  @{ Directory = '\\\\server\\share'; Before = '//SERVER/share/;relative'; After = '//SERVER/share/;relative' }
)
$env:TAPID_EQUIVALENT = 'C:\\Tapid'
foreach ($case in $cases) {
  $global:FixtureDirectory = if ($case.ContainsKey('Directory')) { $case.Directory } else { 'C:\\Tapid' }
  $global:FixturePath = [pscustomobject]@{ Present = $null -ne $case.Before; Value = [string]$case.Before; Kind = 'ExpandString' }
  if (-not $global:FixturePath.Present) { $global:FixturePath.Kind = 'String' }
  $global:FixtureWrites = 0
  $original = Read-UserPath
  $beforeProcess = $env:Path
  & (Join-Path $PSScriptRoot 'install.ps1') -SourceRef fixture -InstallDir $install -ConfigurePath | Out-Null
  if ($global:FixturePath.Value -cne $case.After) { throw "PATH changed incorrectly: $($global:FixturePath.Value)" }
  if ($env:Path -cne $beforeProcess) { throw 'process PATH was silently activated' }
  $writes = $global:FixtureWrites
  & (Join-Path $PSScriptRoot 'install.ps1') -SourceRef fixture -InstallDir $install -ConfigurePath | Out-Null
  if ($global:FixtureWrites -ne $writes) { throw 'repeated install rewrote PATH' }
  $receipt = Join-Path $install '.tapid-path-v1'
  if ([IO.File]::ReadAllText((Join-Path $receipt 'inserted')) -ceq '1') {
    # Recovery on both sides of the registry write.
    [IO.File]::WriteAllText((Join-Path $receipt 'state'), 'pending')
    & (Join-Path $PSScriptRoot 'install.ps1') -SourceRef fixture -InstallDir $install -ConfigurePath | Out-Null
    if ($global:FixtureWrites -ne $writes) { throw 'pending after snapshot was rewritten' }
    [IO.File]::WriteAllText((Join-Path $receipt 'state'), 'pending')
    $global:FixturePath = $original.PSObject.Copy()
    $global:FixtureSuppressWrite = $true
    $refused = $false
    try { & (Join-Path $PSScriptRoot 'install.ps1') -SourceRef fixture -InstallDir $install -ConfigurePath | Out-Null } catch { $refused = $true }
    if (-not $refused -or [IO.File]::ReadAllText((Join-Path $receipt 'state')) -cne 'pending') { throw 'read-back failure did not retain pending recovery' }
    $global:FixtureSuppressWrite = $false
    & (Join-Path $PSScriptRoot 'install.ps1') -SourceRef fixture -InstallDir $install -ConfigurePath | Out-Null
    if ($global:FixturePath.Value -cne $case.After) { throw 'pending before snapshot not recovered' }
    $expected = Read-UserPath
    $global:FixturePath.Value += ';user-edit'
    $refused = $false
    try { & (Join-Path $PSScriptRoot 'uninstall.ps1') -InstallDir $install | Out-Null } catch { $refused = $true }
    if (-not $refused -or -not (Test-Path $receipt) -or -not (Test-Path (Join-Path $install 'tapid.exe'))) { throw 'edited PATH was not preserved with retry state' }
    $global:FixturePath = $expected
    $manifestPath = Join-Path $receipt 'manifest'
    $manifest = [IO.File]::ReadAllText($manifestPath)
    [IO.File]::WriteAllText($manifestPath, 'unknown-version')
    $refused = $false
    try { & (Join-Path $PSScriptRoot 'uninstall.ps1') -InstallDir $install | Out-Null } catch { $refused = $true }
    if (-not $refused -or $global:FixturePath.Value -cne $expected.Value) { throw 'malformed receipt authorized cleanup' }
    [IO.File]::WriteAllText($manifestPath, $manifest)
    $unexpected = Join-Path $receipt 'unknown'
    [IO.File]::WriteAllText($unexpected, 'user data')
    $refused = $false
    try { & (Join-Path $PSScriptRoot 'uninstall.ps1') -InstallDir $install | Out-Null } catch { $refused = $true }
    if (-not $refused -or -not (Test-Path $unexpected)) { throw 'unknown receipt field was removed' }
    Remove-Item -LiteralPath $unexpected
    $saved = $receipt + '.saved'
    Move-Item -LiteralPath $receipt -Destination $saved
    $linkType = if ([IO.Path]::DirectorySeparatorChar -eq '\\') { 'Junction' } else { 'SymbolicLink' }
    New-Item -ItemType $linkType -Path $receipt -Target $saved | Out-Null
    $refused = $false
    try { & (Join-Path $PSScriptRoot 'uninstall.ps1') -InstallDir $install | Out-Null } catch { $refused = $true }
    if (-not $refused -or -not (Test-Path $saved)) { throw 'receipt reparse point was followed' }
    [IO.Directory]::Delete($receipt)
    Move-Item -LiteralPath $saved -Destination $receipt
  }
  Remove-Item -LiteralPath (Join-Path $install 'tapid.exe')
  & (Join-Path $PSScriptRoot 'uninstall.ps1') -InstallDir $install | Out-Null
  if ($global:FixturePath.Present -ne $original.Present -or $global:FixturePath.Value -cne $original.Value -or $global:FixturePath.Kind -cne $original.Kind) { throw 'uninstall did not restore exact PATH snapshot' }
}

Write-Output 'Token preservation verified'
`);
    const result = await run("pwsh", ["-NoProfile", "-File", join(f.directory, "test.ps1")]);
    match(result.stdout, /Token preservation verified/);
  } finally { await f.cleanup(); }
});

test("native Windows PATH persistence resolves a fresh-session executable and restores registry kind", { skip: platform !== "win32" }, async () => {
  const f = await fixture();
  try {
    await writeFile(join(f.directory, "test.ps1"), `$ErrorActionPreference = 'Stop'
function git { $global:LASTEXITCODE = 0 }
function cargo {
    $buildRoot = $args[-1]
    New-Item -ItemType Directory -Force (Join-Path $buildRoot 'bin') | Out-Null
    [IO.File]::Copy($env:TAPID_FIXTURE_EXE, (Join-Path $buildRoot 'bin/tapid.exe'), $true)
    $global:LASTEXITCODE = 0
}
$key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
$present = $key.GetValueNames() -icontains 'Path'
$original = if ($present) { $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) } else { '' }
$kind = if ($present) { $key.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::String }
$processPath = $env:Path
$install = Join-Path $PSScriptRoot "custom ' bin"
try {
    $before = 'relative;;%TAPID_UNRESOLVED_FIXTURE%;'
    $key.SetValue('Path', $before, [Microsoft.Win32.RegistryValueKind]::ExpandString)
    & (Join-Path $PSScriptRoot 'bootstrap-assets/install.ps1') -SourceRef fixture -InstallDir $install | Out-Null
    if ($key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) -cne $before -or $env:Path -cne $processPath) { throw 'no-consent install changed PATH' }
    & (Join-Path $PSScriptRoot 'bootstrap-assets/install.ps1') -SourceRef fixture -InstallDir $install -ConfigurePath | Out-Null
    $persisted = $key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    if ($persisted -cne "$before;$install" -or $key.GetValueKind('Path') -ne [Microsoft.Win32.RegistryValueKind]::ExpandString) { throw 'registry persistence changed raw tokens or kind' }
    # Construct a fresh session solely from persisted values, never add the fixture directory separately.
    $machine = [Environment]::GetEnvironmentVariable('Path', 'Machine')
    $env:Path = [Environment]::ExpandEnvironmentVariables("$machine;$persisted")
    $pwsh = Join-Path $PSHOME 'pwsh.exe'
    $resolved = & $pwsh -NoProfile -Command '(Get-Command tapid -CommandType Application -ErrorAction Stop).Source'
    if ($LASTEXITCODE -ne 0 -or $resolved -cne (Join-Path $install 'tapid.exe')) { throw "wrong fresh-session executable: $resolved" }
    $version = & $pwsh -NoProfile -Command '& tapid --version'
    if ($LASTEXITCODE -ne 0 -or $version -cne $env:TAPID_FIXTURE_VERSION) { throw 'fixture executable did not run by name' }
    & $env:TAPID_FIXTURE_UNINSTALLER -InstallDir $install | Out-Null
    if ($key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames) -cne $before -or $key.GetValueKind('Path') -ne [Microsoft.Win32.RegistryValueKind]::ExpandString) { throw 'cleanup failed to preserve raw registry value and kind' }
    Write-Output 'Native Windows fresh-session resolution verified'
} finally {
    $env:Path = $processPath
    if ($present) { $key.SetValue('Path', $original, $kind) } else { $key.DeleteValue('Path', $false) }
    $key.Dispose()
}
`);
    const result = await run("pwsh", ["-NoProfile", "-File", join(f.directory, "test.ps1")], {
      env: { ...processEnv, TAPID_FIXTURE_EXE: process.execPath, TAPID_FIXTURE_VERSION: process.version, TAPID_FIXTURE_UNINSTALLER: join(root, "scripts/uninstall.ps1") },
    });
    match(result.stdout, /Native Windows fresh-session resolution verified/);
  } finally { await f.cleanup(); }
});
