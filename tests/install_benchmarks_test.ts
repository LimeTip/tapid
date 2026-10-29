import { test } from "node:test";
import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import {
  temporary,
  write,
  writeJson,
  json,
  sha256,
  nodeArgs,
} from "./script-support.ts";
import { readFileSync } from "node:fs";

const command = (args: string[]) =>
  spawnSync(process.execPath, nodeArgs("scripts/benchmark-installs.ts", args), {
    encoding: "utf8",
  });
test("isolated command persists an attempt and summarizes without invoking the tool", async () =>
  temporary(async (root) => {
    const source = join(root, "source");
    write(join(source, "package.json"), "{}");
    const payload = join(root, "fake.cjs");
    write(
      payload,
      `require('node:fs').writeFileSync('observed.json', JSON.stringify({home:process.env.HOME, secret:process.env.BENCH_SECRET})); console.log('installed');`,
    );
    const spec = join(root, "spec.json"),
      run = join(root, "run");
    writeJson(spec, {
      schemaVersion: 1,
      tool: {
        executable: process.execPath,
        sha256: sha256(readFileSync(process.execPath)),
        args: [payload],
        payloads: [{ path: payload, sha256: sha256(readFileSync(payload)) }],
      },
      fixture: {
        source,
        files: [{ path: "package.json", sha256: sha256("{}") }],
      },
      repetitions: 1,
      timeoutMs: 5000,
    });
    process.env.BENCH_SECRET = "must-not-leak";
    try {
      const result = command(["run", spec, run]);
      assert.equal(result.status, 0, result.stderr);
    } finally {
      delete process.env.BENCH_SECRET;
    }
    const record = json(join(run, "attempt-0.json"));
    assert.equal(record.status, "successful");
    assert.equal(record.memory.status, "unavailable");
    assert.equal(record.eligibility.graph, "unverified");
    assert.equal(
      json(join(run, "work-0/project/observed.json")).secret,
      undefined,
    );
    assert.equal(
      json(join(run, "work-0/project/observed.json")).home,
      join(run, "work-0/home"),
    );
    assert.equal(command(["summarize", run]).status, 0);
    assert.equal(json(join(run, "summary.json")).successful, 1);
    assert.notEqual(
      command(["run", spec, run]).status,
      0,
      "must not overwrite observations",
    );
  }));

test("preflight rejects unknown schemas, mismatched identities and escaping inputs before creating a run", async () =>
  temporary(async (root) => {
    const spec = join(root, "spec.json");
    writeJson(spec, { schemaVersion: 999 });
    assert.match(command(["preflight", spec]).stderr, /schemaVersion/);
    const source = join(root, "source");
    write(join(source, "package.json"), "{}");
    const value = {
      schemaVersion: 1,
      tool: {
        executable: process.execPath,
        sha256: "0".repeat(64),
        args: [],
        payloads: [],
      },
      fixture: {
        source,
        files: [{ path: "package.json", sha256: sha256("{}") }],
      },
      repetitions: 1,
      timeoutMs: 1000,
    };
    writeJson(spec, value);
    assert.match(command(["preflight", spec]).stderr, /identity mismatch/);
    value.tool.sha256 = sha256(readFileSync(process.execPath));
    value.fixture.files[0].path = "../outside";
    writeJson(spec, value);
    assert.match(command(["preflight", spec]).stderr, /unsafe fixture path/);
  }));

test("nonzero commands retain diagnostics and cannot become successful timing samples", async () =>
  temporary(async (root) => {
    const source = join(root, "source");
    write(join(source, "package.json"), "{}");
    const spec = join(root, "spec.json"),
      run = join(root, "run");
    writeJson(spec, {
      schemaVersion: 1,
      tool: {
        executable: process.execPath,
        sha256: sha256(readFileSync(process.execPath)),
        args: ["-e", "console.error('intentional failure'); process.exit(7)"],
        payloads: [],
      },
      fixture: {
        source,
        files: [{ path: "package.json", sha256: sha256("{}") }],
      },
      repetitions: 1,
      timeoutMs: 5000,
    });
    assert.notEqual(command(["run", spec, run]).status, 0);
    const record = json(join(run, "attempt-0.json"));
    assert.equal(record.code, 7);
    assert.match(record.stderr, /intentional failure/);
    assert.equal(record.status, "failed");
    const summary = json(join(run, "summary.json"));
    assert.equal(summary.successful, 0);
    assert.equal(summary.failed, 1);
    assert.equal(summary.wallMs.median, null);
  }));

test("summary preserves failures and incomplete attempts using documented quartiles", async () =>
  temporary(async (root) => {
    writeJson(join(root, "manifest.json"), {
      schemaVersion: 1,
      spec: { repetitions: 7 },
    });
    for (const [index, ms] of [1, 2, 3, 4, 10].entries())
      writeJson(join(root, `attempt-${index}.json`), {
        schemaVersion: 1,
        status: "successful",
        elapsedNs: String(ms * 1e6),
      });
    writeJson(join(root, "attempt-5.json"), {
      schemaVersion: 1,
      status: "failed",
      elapsedNs: "1000000",
    });
    assert.equal(command(["summarize", root]).status, 0);
    const summary = json(join(root, "summary.json"));
    assert.equal(summary.wallMs.median, 3);
    assert.equal(summary.wallMs.iqr, 5.5);
    assert.equal(summary.incomplete, 1);
    assert.equal(summary.failed, 1);
  }));

test("timeout retains a failed attempt and stops later repetitions", async () =>
  temporary(async (root) => {
    const source = join(root, "source");
    write(join(source, "package.json"), "{}");
    const spec = join(root, "spec.json"),
      run = join(root, "run");
    writeJson(spec, {
      schemaVersion: 1,
      tool: {
        executable: process.execPath,
        sha256: sha256(readFileSync(process.execPath)),
        args: ["-e", "setInterval(()=>{},1000)"],
        payloads: [],
      },
      fixture: {
        source,
        files: [{ path: "package.json", sha256: sha256("{}") }],
      },
      repetitions: 2,
      timeoutMs: 200,
    });
    assert.match(command(["run", spec, run]).stderr, /schedule aborted/);
    assert.equal(json(join(run, "attempt-0.json")).timedOut, true);
    assert.equal(command(["summarize", run]).status, 0);
    assert.equal(json(join(run, "summary.json")).incomplete, 1);
  }));

test("summary rejects unknown or invalid attempt records instead of publishing misleading statistics", async () =>
  temporary(async (root) => {
    writeJson(join(root, "manifest.json"), {
      schemaVersion: 1,
      spec: { repetitions: 1 },
    });
    writeJson(join(root, "attempt-0.json"), {
      schemaVersion: 1,
      status: "successful",
      elapsedNs: "-1",
    });
    assert.notEqual(command(["summarize", root]).status, 0);
  }));

test("disk evidence counts hard links once per scope and does not follow symlinks", async () =>
  temporary(async (root) => {
    const source = join(root, "source");
    write(join(source, "package.json"), "{}");
    const spec = join(root, "spec.json"),
      run = join(root, "run");
    const program = `const fs=require('node:fs'); fs.writeFileSync('data','1234'); fs.linkSync('data','alias'); fs.symlinkSync(process.env.HOME,'link');`;
    writeJson(spec, {
      schemaVersion: 1,
      tool: {
        executable: process.execPath,
        sha256: sha256(readFileSync(process.execPath)),
        args: ["-e", program],
        payloads: [],
      },
      fixture: {
        source,
        files: [{ path: "package.json", sha256: sha256("{}") }],
      },
      repetitions: 1,
      timeoutMs: 5000,
    });
    const result = command(["run", spec, run]);
    assert.equal(result.status, 0, result.stderr);
    const disk = json(join(run, "attempt-0.json")).disk;
    assert.equal(disk.before.project.logicalBytes, 2);
    assert.equal(disk.after.project.logicalBytes, 6);
    assert.equal(disk.after.project.regularFiles, 3);
    assert.equal(disk.after.project.uniqueFiles, 2);
    assert.equal(disk.after.project.symlinks, 1);
    assert.equal(disk.after.combined.logicalBytes, 6);
  }));

test("each repetition has fresh outputs and cache and bounded retained logs", async () =>
  temporary(async (root) => {
    const source = join(root, "source");
    write(join(source, "package.json"), "{}");
    const spec = join(root, "spec.json"),
      run = join(root, "run");
    const program = `const fs=require('node:fs'); if(fs.existsSync('stale')||fs.existsSync(process.env.XDG_CACHE_HOME+'/stale'))process.exit(9); fs.writeFileSync('stale','x'); fs.writeFileSync(process.env.XDG_CACHE_HOME+'/stale','x'); process.stdout.write('x'.repeat(100000));`;
    writeJson(spec, {
      schemaVersion: 1,
      tool: {
        executable: process.execPath,
        sha256: sha256(readFileSync(process.execPath)),
        args: ["-e", program],
        payloads: [],
      },
      fixture: {
        source,
        files: [{ path: "package.json", sha256: sha256("{}") }],
      },
      repetitions: 2,
      timeoutMs: 5000,
    });
    assert.equal(command(["run", spec, run]).status, 0);
    for (const i of [0, 1]) {
      const attempt = json(join(run, `attempt-${i}.json`));
      assert.equal(attempt.status, "successful");
      assert.equal(attempt.stdout.length, 65536);
      assert.equal(attempt.outputTruncated, true);
    }
  }));

test("unsupported cells persist a reason and never execute or supply timing samples", async () =>
  temporary(async (root) => {
    const source = join(root, "source");
    write(join(source, "package.json"), "{}");
    const spec = join(root, "spec.json"),
      run = join(root, "run");
    writeJson(spec, {
      schemaVersion: 1,
      unsupportedReason: "verified network denial is not provisioned",
      tool: {
        executable: process.execPath,
        sha256: sha256(readFileSync(process.execPath)),
        args: ["-e", "process.exit(99)"],
        payloads: [],
      },
      fixture: {
        source,
        files: [{ path: "package.json", sha256: sha256("{}") }],
      },
      repetitions: 2,
      timeoutMs: 5000,
    });
    assert.equal(command(["run", spec, run]).status, 0);
    const record = json(join(run, "attempt-0.json"));
    assert.equal(record.status, "unsupported");
    assert.match(record.reason, /network denial/);
    assert.equal(record.elapsedNs, null);
    const summary = json(join(run, "summary.json"));
    assert.equal(summary.unsupported, 2);
    assert.equal(summary.successful, 0);
    assert.equal(summary.incomplete, 0);
  }));
