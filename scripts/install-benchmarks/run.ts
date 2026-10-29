import { createHash } from "node:crypto";
import {
  copyFileSync,
  lstatSync,
  mkdirSync,
  readFileSync,
  renameSync,
  writeFileSync,
  readdirSync,
  realpathSync,
} from "node:fs";
import { dirname, isAbsolute, join, relative, resolve } from "node:path";
import { spawn } from "node:child_process";
import { diskSnapshot } from "./disk.ts";

const digest = (path: string) =>
  createHash("sha256").update(readFileSync(path)).digest("hex");
const read = (path: string) => JSON.parse(readFileSync(path, "utf8"));
function atomic(path: string, value: unknown) {
  writeFileSync(path + ".tmp", JSON.stringify(value, null, 2) + "\n", {
    flag: "wx",
  });
  renameSync(path + ".tmp", path);
}
function regular(path: string, root: string) {
  const absolute = resolve(path);
  let cursor = absolute;
  while (true) {
    if (lstatSync(cursor).isSymbolicLink())
      throw new Error("symlink input: " + cursor);
    if (cursor === root) break;
    const parent = dirname(cursor);
    if (parent === cursor) break;
    cursor = parent;
  }
  if (!lstatSync(absolute).isFile())
    throw new Error("not a regular file: " + absolute);
}
export function preflight(path: string) {
  const spec = read(path);
  if (spec.schemaVersion !== 1) throw new Error("unsupported schemaVersion");
  if (
    spec.unsupportedReason !== undefined &&
    (typeof spec.unsupportedReason !== "string" ||
      !spec.unsupportedReason.trim())
  )
    throw new Error("unsupported reason required");
  if (
    !Number.isInteger(spec.repetitions) ||
    spec.repetitions < 1 ||
    spec.repetitions > 100
  )
    throw new Error("invalid repetitions");
  if (
    !Number.isInteger(spec.timeoutMs) ||
    spec.timeoutMs < 1 ||
    spec.timeoutMs > 3600000
  )
    throw new Error("invalid timeout");
  if (
    !isAbsolute(spec.tool.executable) ||
    !Array.isArray(spec.tool.args) ||
    !spec.tool.args.every((v: unknown) => typeof v === "string")
  )
    throw new Error("invalid tool");
  if (!Array.isArray(spec.tool.payloads))
    throw new Error("payload identities required");
  for (const file of [
    { path: spec.tool.executable, sha256: spec.tool.sha256 },
    ...spec.tool.payloads,
  ]) {
    if (!isAbsolute(file.path)) throw new Error("absolute tool path required");
    // Executable symlinks are common, but their resolved bytes are pinned.
    if (digest(file.path) !== file.sha256)
      throw new Error("tool identity mismatch");
  }
  if (
    !isAbsolute(spec.fixture.source) ||
    !Array.isArray(spec.fixture.files) ||
    !spec.fixture.files.length
  )
    throw new Error("invalid fixture");
  spec.fixture.source = realpathSync(spec.fixture.source);
  const paths = new Set();
  for (const file of spec.fixture.files) {
    if (
      typeof file.path !== "string" ||
      !file.path ||
      isAbsolute(file.path) ||
      file.path.split(/[\\/]/).some((v: string) => v === ".." || v === ".") ||
      paths.has(file.path)
    )
      throw new Error("unsafe fixture path");
    paths.add(file.path);
    const source = join(spec.fixture.source, file.path);
    regular(source, spec.fixture.source);
    if (digest(source) !== file.sha256)
      throw new Error("fixture identity mismatch");
  }
  return spec;
}
export async function run(specPath: string, runPath: string) {
  const spec = preflight(specPath);
  if (!isAbsolute(runPath)) throw new Error("absolute run directory required");
  const rel = relative(spec.fixture.source, runPath);
  if (!rel || (!rel.startsWith("..") && !isAbsolute(rel)))
    throw new Error("run directory overlaps fixture");
  // Exclusive creation: no cleanup or overwrite of an existing directory.
  mkdirSync(runPath, { mode: 0o700 });
  atomic(join(runPath, "manifest.json"), {
    schemaVersion: 1,
    spec,
    node: process.version,
    platform: process.platform,
    arch: process.arch,
    timing: "monotonic parent spawn-to-close",
    scope: "development command slice; not a qualified reference benchmark",
  });
  for (let index = 0; index < spec.repetitions; index++) {
    if (spec.unsupportedReason) {
      atomic(join(runPath, `attempt-${index}.json`), {
        schemaVersion: 1,
        index,
        status: "unsupported",
        reason: spec.unsupportedReason,
        elapsedNs: null,
      });
      continue;
    }
    const work = join(runPath, `work-${index}`),
      project = join(work, "project");
    for (const dir of ["project", "home", "cache", "tmp", "config"])
      mkdirSync(join(work, dir), { recursive: true, mode: 0o700 });
    for (const file of spec.fixture.files) {
      const target = join(project, file.path);
      mkdirSync(dirname(target), { recursive: true });
      copyFileSync(join(spec.fixture.source, file.path), target);
    }
    const before = diskSnapshot(project, join(work, "cache"));
    const start = process.hrtime.bigint(),
      startedAt = new Date().toISOString();
    const observation = await new Promise<any>((done) => {
      let stdout = Buffer.alloc(0),
        stderr = Buffer.alloc(0),
        error: string | undefined,
        timedOut = false,
        outputBytes = 0;
      const child = spawn(spec.tool.executable, spec.tool.args, {
        cwd: project,
        env: {
          HOME: join(work, "home"),
          USERPROFILE: join(work, "home"),
          TMPDIR: join(work, "tmp"),
          TMP: join(work, "tmp"),
          TEMP: join(work, "tmp"),
          XDG_CONFIG_HOME: join(work, "config"),
          XDG_CACHE_HOME: join(work, "cache"),
          npm_config_cache: join(work, "cache"),
          npm_config_userconfig: join(work, "config", "npmrc"),
          npm_config_globalconfig: join(work, "config", "global-npmrc"),
        },
        detached: process.platform !== "win32",
        stdio: ["ignore", "pipe", "pipe"],
      });
      child.stdout.on("data", (chunk) => {
        outputBytes += chunk.length;
        stdout = Buffer.concat([stdout, chunk]).subarray(0, 65536);
      });
      child.stderr.on("data", (chunk) => {
        outputBytes += chunk.length;
        stderr = Buffer.concat([stderr, chunk]).subarray(0, 65536);
      });
      child.on("error", (value) => {
        error = value.message;
      });
      const timer = setTimeout(() => {
        timedOut = true;
        try {
          if (process.platform !== "win32" && child.pid)
            process.kill(-child.pid, "SIGKILL");
          else child.kill("SIGKILL");
        } catch {}
        // Pipe closure bounds observation, but does not prove descendant cleanup.
        child.stdout.destroy();
        child.stderr.destroy();
      }, spec.timeoutMs);
      child.on("close", (code, signal) => {
        clearTimeout(timer);
        done({
          code,
          signal,
          error,
          timedOut,
          outputBytes,
          outputTruncated: outputBytes > stdout.length + stderr.length,
          stdout: stdout.toString(),
          stderr: stderr.toString(),
        });
      });
    });
    const elapsedNs = String(process.hrtime.bigint() - start),
      endedAt = new Date().toISOString();
    const after = diskSnapshot(project, join(work, "cache"));
    atomic(join(runPath, `attempt-${index}.json`), {
      schemaVersion: 1,
      index,
      status:
        observation.code === 0 && !observation.timedOut
          ? "successful"
          : "failed",
      startedAt,
      endedAt,
      elapsedNs,
      disk: { before, after },
      command: spec.tool,
      ...observation,
      memory: {
        status: "unavailable",
        reason: "no qualified process-tree collector",
      },
      cacheDownloads: {
        status: "unavailable",
        reason: "no counter integration",
      },
      eligibility: { graph: "unverified", application: "unverified" },
    });
    if (observation.timedOut)
      throw new Error(
        "timeout; descendant cleanup unverified; schedule aborted",
      );
  }
  const summary = summarize(runPath);
  if (summary.failed)
    throw new Error(
      "benchmark commands failed; retained diagnostics in run directory",
    );
}
export function summarize(root: string) {
  const manifest = read(join(root, "manifest.json"));
  if (manifest.schemaVersion !== 1)
    throw new Error("unsupported result schema");
  const attempts = readdirSync(root)
    .filter((name) => /^attempt-\d+\.json$/.test(name))
    .map((name) => read(join(root, name)));
  if (
    !Number.isInteger(manifest.spec.repetitions) ||
    manifest.spec.repetitions < attempts.length
  )
    throw new Error("invalid run manifest");
  for (const attempt of attempts) {
    if (attempt.schemaVersion !== 1) throw new Error("invalid attempt schema");
    if (attempt.status === "unsupported") {
      if (
        typeof attempt.reason !== "string" ||
        !attempt.reason.trim() ||
        attempt.elapsedNs !== null
      )
        throw new Error("invalid unsupported record");
    } else if (
      !["successful", "failed"].includes(attempt.status) ||
      typeof attempt.elapsedNs !== "string" ||
      !/^\d+$/.test(attempt.elapsedNs) ||
      !Number.isFinite(Number(attempt.elapsedNs))
    )
      throw new Error("invalid attempt record");
  }
  const values = attempts
    .filter((a) => a.status === "successful")
    .map((a) => Number(a.elapsedNs) / 1e6)
    .sort((a, b) => a - b);
  const median = (v: number[]): number | null =>
    v.length
      ? (v[Math.floor((v.length - 1) / 2)] + v[Math.floor(v.length / 2)]) / 2
      : null;
  const half = Math.floor(values.length / 2),
    lower = median(values.slice(0, half)),
    upper = median(values.slice(Math.ceil(values.length / 2)));
  const summary = {
    schemaVersion: 1,
    successful: values.length,
    failed: attempts.filter((a) => a.status === "failed").length,
    unsupported: attempts.filter((a) => a.status === "unsupported").length,
    incomplete: manifest.spec.repetitions - attempts.length,
    wallMs: {
      samples: values,
      median: median(values),
      min: values[0] ?? null,
      max: values.at(-1) ?? null,
      iqr: lower === null || upper === null ? null : upper - lower,
    },
    equivalentWork: "unverified",
  };
  // Summary is derived and replaceable; raw observations remain immutable.
  writeFileSync(
    join(root, "summary.json"),
    JSON.stringify(summary, null, 2) + "\n",
  );
  return summary;
}
