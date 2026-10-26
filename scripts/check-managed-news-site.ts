// Required native acceptance orchestration. Rust probes establish security;
// this helper checks the reference application's scripts and checked results.
import assert from "node:assert/strict";
import { spawn, spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { release } from "node:os";
import { closeSync, copyFileSync, cpSync, mkdirSync, openSync, readFileSync, realpathSync, statSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

type Options = {
  source: string;
  binary: string;
  node: string;
  evidence: string;
  readinessMs?: number;
  shutdownMs?: number;
};

type Exit = { code: number | null; signal: string | null; error?: Error };

function launch(binary: string, args: string[], cwd: string, env: NodeJS.ProcessEnv, log: string) {
  const fd = openSync(log, "wx");
  let child;
  try {
    child = spawn(binary, args, { cwd, env, stdio: ["ignore", fd, fd] });
  } finally {
    closeSync(fd);
  }
  const done = new Promise<Exit>(resolve => {
    child.once("error", error => resolve({ code: null, signal: null, error }));
    child.once("close", (code, signal) => resolve({ code, signal }));
  });
  return { child, done };
}

async function deadline<T>(promise: Promise<T>, milliseconds: number, context: string): Promise<T> {
  let timer: ReturnType<typeof setTimeout> | undefined;
  try {
    return await Promise.race([promise, new Promise<never>((_, reject) => {
      timer = setTimeout(() => reject(new Error(`${context} timed out`)), milliseconds);
    })]);
  } finally {
    clearTimeout(timer);
  }
}

function checkedResult(path: string, termination: string) {
  const result = JSON.parse(readFileSync(path, "utf8"));
  assert.equal(result.outcome, "completed");
  assert.equal(result.receipt.assurance, "ManagedTree");
  assert.equal(result.receipt.termination, termination);
  assert.equal(result.receipt.completion.cleanup_confidence, "KernelOwnedComplete");
  assert.deepEqual(result.receipt.requested, result.receipt.enforced);
  return result;
}

async function freePort(): Promise<number> {
  const listener = createServer();
  await new Promise<void>((resolve, reject) => {
    listener.once("error", reject);
    listener.listen(0, "127.0.0.1", resolve);
  });
  const address = listener.address();
  assert.ok(address && typeof address !== "string");
  await new Promise<void>((resolve, reject) => listener.close(error => error ? reject(error) : resolve()));
  return address.port;
}

const sleep = (ms: number) => new Promise(resolve => setTimeout(resolve, ms));

export async function runManagedNewsSite(options: Options) {
  const evidence = resolve(options.evidence);
  mkdirSync(evidence, { recursive: true });
  // Keep the isolated project, exact policy, result files and logs together.
  // Never replace the portable fixture's Restricted policy in the checkout.
  const project = join(evidence, "project");
  mkdirSync(project);
  cpSync(options.source, project, { recursive: true, filter: source => !["node_modules", ".next", ".tmp", "tapid.lock"].includes(source.split(/[\\/]/).at(-1)!) });
  copyFileSync(join(project, "tapid.linux-managed.toml"), join(project, "tapid.toml"));
  copyFileSync(join(project, "tapid.toml"), join(evidence, "policy.toml"));
  for (const path of [".next", ".tmp", ".tmp/home"]) mkdirSync(join(project, path), { recursive: true });
  const binary = realpathSync(options.binary), node = realpathSync(options.node);
  const env = {
    PATH: dirname(node), HOME: join(project, ".tmp/home"),
    TMPDIR: join(project, ".tmp"), NEXT_TELEMETRY_DISABLED: "1",
  };
  const summary: any = { status: "running", project, binary, node, commands: [] };
  const version = spawnSync(node, ["--version"], { env, encoding: "utf8", timeout: 10000 });
  assert.equal(version.status, 0, "selected Node version probe failed");
  assert.match(version.stdout.trim(), /^v22\./, "acceptance requires Node 22");
  summary.runtime = {
    version: version.stdout.trim(), platform: process.platform, architecture: process.arch, kernel: release(),
    node_sha256: createHash("sha256").update(readFileSync(node)).digest("hex"),
    tapid_sha256: createHash("sha256").update(readFileSync(binary)).digest("hex"),
  };
  const save = () => writeFileSync(join(evidence, "summary.json"), JSON.stringify(summary, null, 2) + "\n");
  const shutdownMs = options.shutdownMs ?? 15000;
  async function stop(process: ReturnType<typeof launch>) {
    process.child.kill("SIGINT");
    try {
      return await deadline(process.done, shutdownMs, "ManagedTree cancellation");
    } catch (error) {
      // Emergency caller termination is an error, never successful cleanup.
      // The runner's parent-death boundary remains responsible for its tree.
      process.child.kill("SIGKILL");
      await deadline(process.done, shutdownMs, "caller termination");
      throw error;
    }
  }
  async function command(name: string, args: string[], expected = 0) {
    const record: any = { name, binary, args };
    summary.commands.push(record);
    const started = Date.now();
    save();
    const process = launch(binary, args, project, env, join(evidence, `${name}.log`));
    let result: Exit;
    try {
      result = await deadline(process.done, 360000, name);
    } catch (error) {
      await stop(process);
      throw error;
    }
    Object.assign(record, {elapsed_ms: Date.now() - started, log_bytes: statSync(join(evidence, `${name}.log`)).size, exit_code: result.code, signal: result.signal});
    if (result.error) throw result.error;
    assert.equal(result.code, expected, `${name} exited ${result.code} (${result.signal}); see ${name}.log`);
  }
  const runArgs = (script: string) => ["run", script, "--project-dir", project, "--node-runtime", node, "--result-file", join(evidence, `${script}.json`)];
  save();
  try {
    await command("install", ["install", "--project-dir", project]);
    await command("build", [...runArgs("build"), "--allow-process-memory-stats"]);
    summary.build = checkedResult(join(evidence, "build.json"), "Exited(0)");
    await command("test", runArgs("test"));
    summary.test = checkedResult(join(evidence, "test.json"), "Exited(0)");
    const port = await freePort();
    const args = [...runArgs("start"), "--", "--hostname", "127.0.0.1", "--port", String(port)];
    summary.commands.push({ name: "start", binary, args });
    save();
    const server = launch(binary, args, project, env, join(evidence, "start.log"));
    const url = `http://127.0.0.1:${port}/acceptance`;
    let polling = true;
    try {
      await deadline((async () => {
        while (polling) {
          const exited = await Promise.race([server.done, sleep(0).then(() => null)]);
          assert.equal(exited, null, `start exited before readiness: ${JSON.stringify(exited)}`);
          try {
            const response = await fetch(url, { signal: AbortSignal.timeout(1000) });
            if (response.ok && (await response.text()).trim() === "TAPID_NEWS_SITE_ACCEPTANCE_V1") break;
          } catch { /* Startup is asynchronous; poll until the bounded readiness deadline. */ }
          await sleep(100);
        }
      })(), options.readinessMs ?? 60000, "HTTP readiness");
      writeFileSync(join(evidence, "acceptance.txt"), "TAPID_NEWS_SITE_ACCEPTANCE_V1\n");
    } finally {
      polling = false;
      const result = await stop(server);
      assert.equal(result.code, 130, `start cancellation exited ${result.code} (${result.signal})`);
      summary.start = checkedResult(join(evidence, "start.json"), "Cancelled");
    }
    let stillServing = false;
    try {
      await fetch(url, { signal: AbortSignal.timeout(1000) });
      stillServing = true;
    } catch { /* Refusal after checked authoritative cleanup is expected. */ }
    assert.equal(stillServing, false, "acceptance server survived cancellation");
    summary.status = "passed";
    return summary;
  } catch (error) {
    summary.status = "failed";
    summary.error = String(error);
    throw error;
  } finally {
    save();
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(resolve(process.argv[1])).href) {
  const { values } = parseArgs({ options: { binary: { type: "string" }, node: { type: "string" }, evidence: { type: "string" } } });
  assert.ok(values.binary && values.node && values.evidence, "--binary, --node and --evidence are required");
  assert.equal(process.platform, "linux");
  assert.equal(process.arch, "x64");
  assert.equal(process.versions.node.split(".")[0], "22");
  await runManagedNewsSite({ source: fileURLToPath(new URL("../examples/news-site-consumer", import.meta.url)), binary: values.binary, node: values.node, evidence: values.evidence });
}
