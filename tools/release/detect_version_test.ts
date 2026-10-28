import { strictEqual as equal } from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";

const script = fileURLToPath(new URL("detect-version.sh", import.meta.url));
test("shell detection accepts only a complete successful stable version", { skip: process.platform === "win32" }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "tapid detection "));
  const executable = join(directory, "tapid fixture");
  try {
    await writeFile(executable, "#!/bin/sh\nprintf 'tapid 1.2.3\\n'\n");
    await chmod(executable, 0o755);
    const result = spawnSync("/bin/sh", [script, executable, "1.2.3"], { encoding: "utf8" });
    equal(result.status, 0, result.stderr);
    equal(result.stdout, "");
    equal(result.stderr, "");
  } finally { await rm(directory, { recursive: true, force: true }); }
});

const pwshAvailable = spawnSync("pwsh", ["-NoProfile", "-NonInteractive", "-Command", "exit 0"]).status === 0;
test("PowerShell detection bounds a hung executable and inherited output pipes", {
  skip: !pwshAvailable || process.platform === "win32" ? "requires pwsh and Unix executable fixtures" : false,
}, async () => {
  const directory = await mkdtemp(join(tmpdir(), "tapid detection timeout "));
  const executable = join(directory, "tapid fixture");
  const pidFile = join(directory, "child-pid");
  const home = join(directory, "home");
  await mkdir(home);
  try {
    // Exclude PowerShell's first-run home initialization from the detector deadline.
    equal(spawnSync("pwsh", ["-NoProfile", "-NonInteractive", "-Command", "exit 0"], {
      env: { ...process.env, HOME: home }, timeout: 30_000,
    }).status, 0);
    for (const retainedPipe of [false, true]) {
      // Both fixtures have a finite lifetime even if detector cleanup fails.
      await writeFile(executable, `#!/bin/sh\nsleep 20 &\nchild=$!\nprintf '%s' "$child" > '${pidFile.replaceAll("'", "'\\''")}'\n${retainedPipe ? "printf 'tapid 1.2.3\\n'\nexit 0" : 'wait "$child"'}\n`);
      await chmod(executable, 0o755);
      const started = performance.now();
      try {
        const result = spawnSync("pwsh", ["-NoProfile", "-NonInteractive", "-File", script.replace(/\.sh$/, ".ps1"), "-ExecutablePath", executable, "-ExpectedVersion", "1.2.3"], {
          encoding: "utf8", timeout: 10_000, cwd: home, env: { ...process.env, HOME: home },
        });
        equal(result.error, undefined, "detector must return before the outer watchdog");
        equal(result.status, 1, result.stderr);
        equal(result.stdout, "");
        equal(result.stderr, "tapid detection: missing, invalid, failed, or mismatched version\n");
        equal(performance.now() - started < 9_000, true, "deadline plus cleanup/startup margin");
      } finally {
        const pid = Number(await readFile(pidFile, "utf8"));
        try { process.kill(pid, "SIGKILL"); } catch (error) {
          if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
        }
      }
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
});
for (const engine of ["shell", "PowerShell"] as const) {
  test(`${engine} detection rejects invalid arguments and output without normalization`, {
    skip: engine === "shell" ? (process.platform === "win32" ? "POSIX fixture requires Unix" : false) : (!pwshAvailable ? "pwsh unavailable" : process.platform === "win32" ? "Unix executable fixtures; native failure probe runs separately" : false),
  }, async () => {
    const directory = await mkdtemp(join(tmpdir(), "tapid detection "));
    const executable = join(directory, "tapid fixture");
    const home = join(directory, "home");
    await mkdir(home);
    // PowerShell initializes its own per-user directories before running scripts.
    if (engine === "PowerShell") {
      equal(spawnSync("pwsh", ["-NoProfile", "-NonInteractive", "-Command", "exit 0"], {
        env: { ...process.env, HOME: home }, timeout: 30_000,
      }).status, 0);
    }
    const homeEntries = (await readdir(home)).sort().join("\n");
    const invoke = (path = executable, version = "1.2.3", extra: string[] = []) => spawnSync(
      engine === "shell" ? "/bin/sh" : "pwsh",
      engine === "shell" ? [script, path, version, ...extra] : ["-NoProfile", "-NonInteractive", "-File", script.replace(/\.sh$/, ".ps1"), "-ExecutablePath", path, "-ExpectedVersion", version, ...extra],
      { encoding: "utf8", cwd: home, env: { ...process.env, HOME: home }, timeout: 10_000 },
    );
    const failure = (result: ReturnType<typeof invoke>) => {
      equal(result.status, 1, result.stderr);
      equal(result.stdout, "");
      equal(result.stderr.length > 0, true);
    };
    try {
      for (const output of ["tapid 1.2.3", "tapid 1.2.3\n", "tapid 1.2.3\r\n"]) {
        await writeFile(executable, `#!/bin/sh\nprintf '%b' '${output.replaceAll("\0", "\\000")}'\n`);
        await chmod(executable, 0o755);
        const result = invoke();
        equal(result.status, 0, result.stderr);
        equal(result.stdout + result.stderr, "");
      }
      for (const output of ["", "\uFEFFtapid 1.2.3\n", "tapid 1.2.3\n\n", "tapid 1.2.3\r\n\r\n", "tapid 1.2.3\nextra", " tapid 1.2.3", "tapid 1.2.3 ", "prefix tapid 1.2.3", "tapid 1.2.3-beta", "tapid 1.2.3+build", "tapid 01.2.3", "tapid 1.2.3\r", "tapid 1.2.3\0"]) {
        await writeFile(executable, `#!/bin/sh\nprintf '%b' '${output.replaceAll("\0", "\\000")}'\n`);
        failure(invoke());
      }
      await writeFile(executable, "#!/bin/sh\nprintf 'tapid 1.2.3\\n'\n");
      for (const version of ["", "1.2", "01.2.3", "1.02.3", "1.2.03", "1.2.3-beta", "1.2.3+build", "1.2.3\n", "1.2.4"]) failure(invoke(executable, version));
      failure(invoke("relative"));
      failure(invoke(join(directory, "missing")));
      failure(invoke(executable, "1.2.3", ["extra"]));
      const missing = spawnSync(engine === "shell" ? "/bin/sh" : "pwsh", engine === "shell" ? [script] : ["-NoProfile", "-NonInteractive", "-File", script.replace(/\.sh$/, ".ps1")], { encoding: "utf8" });
      failure(missing);
      await writeFile(executable, "#!/bin/sh\nprintf 'tapid 1.2.3\\n'\nexit 7\n");
      failure(invoke());
      await chmod(executable, 0o644);
      failure(invoke());
      equal((await readdir(home)).sort().join("\n"), homeEntries, "detection added entries to the isolated home/project");
    } finally { await rm(directory, { recursive: true, force: true }); }
  });
}

test("native Windows PowerShell handles nonzero native status and invocation exception", { skip: process.platform !== "win32" || !pwshAvailable }, async () => {
  const directory = await mkdtemp(join(tmpdir(), "tapid detection "));
  try {
    // A copied native command cannot emit Tapid's version; --version also fails.
    const { copyFile } = await import("node:fs/promises");
    const executable = join(directory, "probe.exe");
    await copyFile(join(process.env.SystemRoot!, "System32", "cmd.exe"), executable);
    const invalid = join(directory, "invalid.exe");
    await writeFile(invalid, "not an executable image");
    for (const path of [executable, invalid, join(directory, "missing.exe")]) {
      const result = spawnSync("pwsh", ["-NoProfile", "-NonInteractive", "-File", script.replace(/\.sh$/, ".ps1"), "-ExecutablePath", path, "-ExpectedVersion", "1.2.3"], { encoding: "utf8", input: "exit 7\r\n", timeout: 10_000 });
      equal(result.status, 1, result.stderr);
      equal(result.stdout, "");
    }
  } finally { await rm(directory, { recursive: true, force: true }); }
});
