// Execute the literal Unix documentation vocabulary with bounded processes and provenance checks.
import { createHash } from "node:crypto";
import { spawn } from "node:child_process";
import {
  closeSync,
  copyFileSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  openSync,
  readFileSync,
  readSync,
  realpathSync,
  rmSync,
  statSync,
  writeFileSync,
} from "node:fs";
import { arch, tmpdir } from "node:os";
import {
  basename,
  delimiter,
  dirname,
  isAbsolute,
  join,
  relative,
  resolve,
  sep,
} from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { parseArgs } from "node:util";

type ProcessResult = {
  exit_code: number | null;
  output: string;
  failure_class: string | null;
};
type Receipt = {
  command: string;
  exit_code: number | null;
  output: string;
  failure_class?: string | null;
};
type Report = {
  schema_version: number;
  example: string;
  status: string;
  commands: Receipt[];
  assertions: { id: string; passed: boolean }[];
  [key: string]: any;
};
type Options = {
  timeout?: number;
  output_limit?: number;
  assertions?: string[];
  upgrade_target?: { sha256: string; version: string };
  expected_exit?: number;
  expected_output?: string;
};
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const within = (parent: string, child: string) => {
  const path = relative(parent, child);
  return path !== ".." && !path.startsWith(".." + sep) && !isAbsolute(path);
};
export function readBounded(path: string, limit = 1048576): Buffer {
  const fd = openSync(path, "r");
  try {
    const data = Buffer.alloc(limit + 1);
    let count = 0,
      bytes = 0;
    while (
      count <= limit &&
      (bytes = readSync(fd, data, count, data.length - count, null)) > 0
    )
      count += bytes;
    if (count > limit) throw new Error("file exceeds size limit: " + path);
    return data.subarray(0, count);
  } finally {
    closeSync(fd);
  }
}
export const digest = (path: string) =>
  createHash("sha256").update(readFileSync(path)).digest("hex");
export function cargoExecutable(output: string, target: string): string {
  const artifacts = new Set<string>();
  for (const line of output
    .split("\n")
    .filter((line) => line.startsWith("{"))) {
    const item = JSON.parse(line);
    if (
      item.reason === "compiler-artifact" &&
      item.target?.name === "tapid" &&
      item.target?.kind?.includes("bin") &&
      item.executable
    ) {
      const path = realpathSync(item.executable);
      if (!within(realpathSync(target), path) || !statSync(path).isFile())
        throw new Error(
          "Cargo executable is outside dedicated target or missing",
        );
      artifacts.add(path);
    }
  }
  if (artifacts.size !== 1)
    throw new Error("Cargo did not identify exactly one Tapid executable");
  return [...artifacts][0];
}

export function boundedProcess(
  args: string[],
  options: {
    cwd: string;
    env: NodeJS.ProcessEnv;
    timeout?: number;
    output_limit?: number;
  },
): Promise<ProcessResult> {
  const { cwd, env, timeout = 120, output_limit = 65536 } = options;
  return new Promise((resolveResult, reject) => {
    const child = spawn(args[0], args.slice(1), {
      cwd,
      env,
      detached: process.platform !== "win32",
      stdio: ["ignore", "pipe", "pipe"],
    });
    let failure: string | null = null,
      size = 0;
    const chunks: Buffer[] = [];
    let terminationRequested = false;
    const kill = () => {
      if (terminationRequested || !child.pid) return;
      terminationRequested = true;
      try {
        if (process.platform === "win32") child.kill("SIGKILL");
        else process.kill(-child.pid, "SIGKILL");
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
      }
    };
    const timer = setTimeout(() => {
      failure ??= "timeout";
      kill();
    }, timeout * 1000);
    const collect = (chunk: Buffer) => {
      const remaining = Math.max(0, output_limit - size);
      chunks.push(chunk.subarray(0, remaining));
      size += chunk.length;
      if (size > output_limit && !failure) {
        failure = "output-limit";
        kill();
      }
    };
    child.stdout.on("data", collect);
    child.stderr.on("data", collect);
    child.once("error", (error) => {
      clearTimeout(timer);
      reject(error);
    });
    child.once("close", (code) => {
      clearTimeout(timer);
      kill();
      resolveResult({
        exit_code: failure ? null : code,
        output: Buffer.concat(chunks).toString("utf8"),
        failure_class: failure,
      });
    });
  });
}

export async function runExample(
  script: string,
  binary: string,
  expectedDigest: string,
  expectedVersion: string,
  options: Options = {},
): Promise<Report> {
  const report: Report = {
    schema_version: 1,
    example: basename(script, ".sh"),
    status: "failed",
    commands: [],
    assertions: [],
  };
  try {
    return await executeExample(
      report,
      script,
      binary,
      expectedDigest,
      expectedVersion,
      options,
    );
  } catch (error) {
    return Object.assign(report, {
      status: "failed",
      failure_class: "execution",
      error: String(error).slice(0, 2000),
    });
  }
}
async function executeExample(
  report: Report,
  script: string,
  binary: string,
  expectedDigest: string,
  expectedVersion: string,
  options: Options,
): Promise<Report> {
  const {
    timeout = 120,
    output_limit = 65536,
    assertions = [],
    upgrade_target,
    expected_exit = 0,
    expected_output,
  } = options;
  report.status = "passed";
  const text = new TextDecoder("utf-8", { fatal: true }).decode(
    readBounded(script, 32768),
  );
  if (text.includes("\r") || text.includes("\0"))
    throw new Error("examples require LF text without NUL");
  report.script_sha256 = digest(script);
  const commands = text
    .split("\n")
    .filter((line) => line.trim() && !line.startsWith("#"));
  const allowed =
    /^(?:mkdir [a-z][a-z0-9-]*|cd [a-z][a-z0-9-]*|tapid (?:init(?: [a-z][a-z0-9-]*)?|(?:i|install)(?: is-char)?(?: --offline)?(?: --frozen)?|--version|upgrade(?: --dry-run| --help)?))$/;
  if (
    !commands.length ||
    commands.length > 16 ||
    commands.some((line) => !allowed.test(line))
  )
    return Object.assign(report, {
      status: "failed",
      failure_class: "contract",
      error: "unsupported command vocabulary",
    });
  if (
    expected_exit &&
    (commands.length !== 1 || commands[0] !== "tapid upgrade --help")
  )
    throw new Error(
      "negative outcome only supported for upgrade capability probe",
    );
  if (commands.includes("tapid upgrade") && !upgrade_target)
    throw new Error(
      "upgrade requires an explicit expected target digest and version",
    );
  const temporary = mkdtempSync(join(tmpdir(), "tapid-doc-example-"));
  try {
    const home = join(temporary, "home"),
      bindir = join(temporary, "bin"),
      installed = join(bindir, "tapid");
    mkdirSync(home);
    mkdirSync(bindir);
    copyFileSync(binary, installed);
    if (upgrade_target)
      writeFileSync(join(bindir, ".tapid-managed"), "tapid-managed-v1\n");
    if (digest(installed) !== expectedDigest)
      return Object.assign(report, {
        status: "failed",
        failure_class: "provenance",
        error: "binary digest mismatch",
      });
    const env = {
      HOME: home,
      PATH: [bindir, "/usr/bin", "/bin"].join(delimiter),
      XDG_CACHE_HOME: join(home, ".cache"),
      TMPDIR: temporary,
      LANG: "C",
      SHELL: "/bin/sh",
    };
    const version = await boundedProcess([installed, "--version"], {
      cwd: temporary,
      env,
      timeout: 10,
      output_limit,
    });
    report.version_probe = version;
    if (
      version.failure_class ||
      version.exit_code !== 0 ||
      version.output.trim() !== expectedVersion
    )
      return Object.assign(report, {
        status: "failed",
        failure_class: "provenance",
        error: "binary version mismatch",
      });
    let cwd = join(temporary, "project");
    mkdirSync(cwd);
    for (const command of commands) {
      const args = command.split(" "),
        frozen = args[0] === "tapid" && args.includes("--frozen"),
        lock = join(cwd, "tapid.lock");
      const before =
        frozen && existsSync(lock) ? readBounded(lock, 8388608) : undefined;
      let code: number | null, output: string;
      if (args[0] === "cd") {
        const target = join(cwd, args[1]);
        code = existsSync(target) && statSync(target).isDirectory() ? 0 : 1;
        output = "";
        if (code === 0) cwd = target;
      } else {
        const result = await boundedProcess(
          [args[0] === "tapid" ? installed : args[0], ...args.slice(1)],
          { cwd, env, timeout, output_limit },
        );
        code = result.exit_code;
        output = result.output;
        if (result.failure_class) {
          report.commands.push({ command, ...result });
          Object.assign(report, {
            status: "failed",
            failure_class: result.failure_class,
          });
          break;
        }
      }
      report.commands.push({ command, exit_code: code, output });
      if (command === "tapid upgrade" && code === 0) {
        expectedDigest = upgrade_target!.sha256;
        expectedVersion = upgrade_target!.version;
        if (digest(installed) !== expectedDigest) {
          Object.assign(report, {
            status: "failed",
            failure_class: "provenance",
            error: "upgrade target digest mismatch",
          });
          break;
        }
        const after = await boundedProcess([installed, "--version"], {
          cwd,
          env,
          timeout,
          output_limit,
        });
        if (
          after.failure_class ||
          after.exit_code !== 0 ||
          after.output.trim() !== expectedVersion
        ) {
          Object.assign(report, {
            status: "failed",
            failure_class: "provenance",
            error: "upgrade target version mismatch",
          });
          break;
        }
        report.binary_after = {
          sha256: digest(installed),
          version: after.output.trim(),
        };
      }
      if (
        digest(installed) !== expectedDigest ||
        realpathSync(join(bindir, "tapid")) !== realpathSync(installed)
      ) {
        Object.assign(report, {
          status: "failed",
          failure_class: "provenance",
          error: "executable changed",
        });
        break;
      }
      if (
        code !== expected_exit ||
        (expected_output && !output.includes(expected_output))
      ) {
        Object.assign(report, { status: "failed", failure_class: "command" });
        break;
      }
      if (
        frozen &&
        (!before ||
          !existsSync(lock) ||
          !readBounded(lock, 8388608).equals(before))
      ) {
        Object.assign(report, {
          status: "failed",
          failure_class: "assertion",
          error: "frozen lockfile changed or missing",
        });
        break;
      }
    }
    if (report.status === "passed")
      for (const assertion of assertions) {
        let passed: boolean;
        switch (assertion) {
          case "manifest":
            passed = existsSync(join(cwd, "package.json"));
            break;
          case "is-char-installed": {
            const manifest = JSON.parse(
                readBounded(join(cwd, "package.json")).toString(),
              ),
              pkg = JSON.parse(
                readBounded(
                  join(cwd, "node_modules/is-char/package.json"),
                ).toString(),
              );
            passed =
              Object.hasOwn(manifest.dependencies ?? {}, "is-char") &&
              pkg.name === "is-char" &&
              existsSync(join(cwd, "tapid.lock"));
            break;
          }
          case "upgrade-provenance": {
            const path = join(bindir, ".tapid-release-state.json"),
              state = existsSync(path)
                ? JSON.parse(readBounded(path).toString())
                : {};
            report.upgrade_state = state;
            passed =
              state.schema === "tapid-release-state-v2" &&
              ["signature", "checksum"].includes(state.verification) &&
              state.last_known_good?.version ===
                expectedVersion.replace(/^tapid /, "") &&
              /^[a-f0-9]{64}$/.test(
                state.last_known_good?.artifact_sha256 ?? "",
              );
            break;
          }
          case "frozen-lockfile-unchanged":
            passed = report.commands.some(
              (item) =>
                item.command.includes("--frozen") && item.exit_code === 0,
            );
            break;
          default:
            throw new Error("unknown assertion: " + assertion);
        }
        report.assertions.push({ id: assertion, passed });
        if (!passed)
          Object.assign(report, {
            status: "failed",
            failure_class: "assertion",
            error: assertion,
          });
      }
    return report;
  } finally {
    rmSync(temporary, { recursive: true, force: true });
  }
}

export async function main(argv = process.argv.slice(2)): Promise<number> {
  const { values: args } = parseArgs({
    args: argv,
    options: Object.fromEntries(
      [
        "lane",
        "report",
        "binary",
        "expected-sha256",
        "expected-version",
        "release-tag",
        "release-source-sha",
        "upgrade-target-sha256",
        "upgrade-target-version",
      ]
        .map((name) => [name, { type: "string" }])
        .concat([
          ["example", { type: "string", multiple: true }],
          ["allow-network", { type: "boolean" }],
        ]),
    ) as any,
  });
  if (
    !["source", "published"].includes(args.lane as string) ||
    !args.report ||
    !args.example
  )
    throw new Error("--lane, --example and --report are required");
  const report: any = {
    schema_version: 1,
    lane: args.lane,
    source_sha: null,
    release_tag: args["release-tag"] ?? null,
    release_source_sha: args["release-source-sha"] ?? null,
    platform: process.platform === "win32" ? "win32" : process.platform,
    machine: arch() === "arm64" ? "arm64" : "x86_64",
    status: "failed",
    examples: [],
  };
  let target: string | undefined;
  try {
    const inventory = JSON.parse(
      readBounded(join(root, "docs/examples/contracts.json")).toString(),
    );
    if (inventory.schema_version !== 1)
      throw new Error("unsupported contract schema");
    const entries = new Map(
      inventory.examples.map((entry) => [entry.id, entry]),
    );
    if (entries.size !== inventory.examples.length)
      throw new Error("duplicate example ID");
    let upgrade_target: Options["upgrade_target"];
    if (args["upgrade-target-sha256"] || args["upgrade-target-version"]) {
      if (
        !/^[a-f0-9]{64}$/.test(
          (args["upgrade-target-sha256"] as string) ?? "",
        ) ||
        !/^tapid \d+\.\d+\.\d+$/.test(
          (args["upgrade-target-version"] as string) ?? "",
        )
      )
        throw new Error("upgrade target requires exact digest and version");
      upgrade_target = {
        sha256: args["upgrade-target-sha256"] as string,
        version: args["upgrade-target-version"] as string,
      };
    }
    const selected: [any, string][] = (args.example as string[]).map((name) => {
      const entry: any = entries.get(name);
      if (!entry) throw new Error("unknown example: " + name);
      if (
        !entry.lanes.includes(args.lane) ||
        !entry.platforms.includes(process.platform)
      )
        throw new Error("example unavailable for lane/platform: " + name);
      if (entry.network && !args["allow-network"])
        throw new Error("network example requires --allow-network: " + name);
      const path = realpathSync(join(root, entry.file));
      if (!within(join(root, "docs/examples"), path) || !path.endsWith(".sh"))
        throw new Error("invalid example path");
      return [entry, path];
    });
    const source = await boundedProcess(["git", "rev-parse", "HEAD"], {
      cwd: root,
      env: process.env,
      timeout: 10,
    });
    if (source.failure_class || source.exit_code !== 0)
      throw new Error("cannot identify source revision");
    report.source_sha = source.output.trim();
    const dirty = await boundedProcess(
      ["git", "status", "--porcelain", "--untracked-files=all"],
      { cwd: root, env: process.env, timeout: 10 },
    );
    if (dirty.failure_class || dirty.exit_code !== 0)
      throw new Error("cannot identify source tree state");
    report.source_dirty = Boolean(dirty.output);
    let binary: string, expectedDigest: string, expectedVersion: string;
    if (args.lane === "source") {
      if (args.binary || args["expected-sha256"] || args["expected-version"])
        throw new Error(
          "source lane builds its own artifact; binary overrides prohibited",
        );
      target = mkdtempSync(join(tmpdir(), "tapid-doc-build-"));
      const build = await boundedProcess(
        [
          "cargo",
          "build",
          "--locked",
          "--bin",
          "tapid",
          "--message-format=json-render-diagnostics",
          "--target-dir",
          target,
        ],
        { cwd: root, env: process.env, timeout: 600, output_limit: 8388608 },
      );
      if (build.failure_class || build.exit_code !== 0) {
        report.build = build;
        throw new Error("source build failed; no fallback executable");
      }
      binary = cargoExecutable(build.output, target);
      expectedDigest = digest(binary);
      const metadata = await boundedProcess(
        [
          "cargo",
          "metadata",
          "--no-deps",
          "--format-version",
          "1",
          "--locked",
          "--offline",
        ],
        { cwd: root, env: process.env, timeout: 60, output_limit: 8388608 },
      );
      if (metadata.failure_class || metadata.exit_code !== 0)
        throw new Error("cannot identify CLI package version");
      const records = metadata.output
        .split("\n")
        .filter((line) => line.startsWith("{"));
      if (records.length !== 1)
        throw new Error("cannot identify Cargo metadata");
      const cli = JSON.parse(records[0]).packages.filter(
        (pkg) => pkg.name === "tapid",
      );
      if (cli.length !== 1)
        throw new Error("cannot identify CLI package version");
      expectedVersion = "tapid " + cli[0].version;
    } else {
      if (
        !args.binary ||
        !/^[a-f0-9]{64}$/.test((args["expected-sha256"] as string) ?? "")
      )
        throw new Error("published lane requires binary and expected SHA-256");
      if (!/^v\d+\.\d+\.\d+$/.test((args["release-tag"] as string) ?? ""))
        throw new Error("published lane requires an exact stable release tag");
      if (
        args["expected-version"] !==
        "tapid " + (args["release-tag"] as string).slice(1)
      )
        throw new Error("release tag and expected version disagree");
      binary = realpathSync(args.binary as string);
      expectedDigest = args["expected-sha256"] as string;
      expectedVersion = args["expected-version"] as string;
    }
    report.binary = {
      path: binary,
      sha256: expectedDigest,
      version: expectedVersion,
    };
    for (const [entry, script] of selected) {
      let outcome: any = { exit_code: 0 };
      if (args.lane === "published" && entry.release_expectations) {
        if (
          !Object.hasOwn(
            entry.release_expectations,
            args["release-tag"] as string,
          )
        )
          throw new Error(
            "release capability expectation needs review: " +
              args["release-tag"],
          );
        outcome = entry.release_expectations[args["release-tag"] as string];
      }
      if (outcome.skip) {
        report.examples.push({
          example: entry.id,
          status: "skipped",
          reason: outcome.skip,
          commands: [],
          assertions: [],
        });
        continue;
      }
      const result = await runExample(
        script,
        binary,
        expectedDigest,
        expectedVersion,
        {
          assertions: entry.assertions,
          upgrade_target,
          expected_exit: outcome.exit_code,
          expected_output: outcome.output_contains,
        },
      );
      report.examples.push(result);
      if (result.status !== "passed") break;
    }
    report.status =
      report.examples.length === selected.length &&
      report.examples.every((entry) =>
        ["passed", "skipped"].includes(entry.status),
      )
        ? "passed"
        : "failed";
    if (
      report.status === "passed" &&
      report.examples.some((entry) => entry.status === "skipped")
    )
      report.status = "skipped";
  } catch (error) {
    Object.assign(report, {
      status: "failed",
      failure_class: "execution",
      error: String(error).slice(0, 2000),
    });
  } finally {
    if (target) rmSync(target, { recursive: true, force: true });
  }
  mkdirSync(dirname(args.report as string), { recursive: true });
  writeFileSync(args.report as string, JSON.stringify(report, null, 2) + "\n");
  console.log(JSON.stringify({ status: report.status, report: args.report }));
  return ["passed", "skipped"].includes(report.status) ? 0 : 1;
}
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
)
  main()
    .then((code) => {
      process.exitCode = code;
    })
    .catch((error) => {
      console.error(String(error));
      process.exitCode = 2;
    });
