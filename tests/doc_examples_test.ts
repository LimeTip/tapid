import { strict as assert } from "node:assert";
import { test } from "node:test";
import { join } from "node:path";
import {
  chmodSync,
  existsSync,
  mkdirSync,
  readdirSync,
  realpathSync,
} from "node:fs";
import { spawnSync } from "node:child_process";
import {
  runExample,
  digest,
  boundedProcess,
  cargoExecutable,
} from "../scripts/check-doc-examples.ts";
import {
  temporary,
  write,
  root,
  read,
  json,
  quote,
  sha256,
  nodeArgs,
  hasCommand,
} from "./script-support.ts";

async function fixtureRun(
  commands: string,
  body = "exit 0",
  options: any = {},
) {
  return temporary(async (path) => {
    const binary = join(path, "fixture"),
      script = join(path, "example.sh");
    write(
      binary,
      '#!/bin/sh\nif [ "$1" = --version ]; then echo "tapid 1.2.3"; exit; fi\n' +
        body +
        "\n",
    );
    chmodSync(binary, 0o755);
    write(script, commands);
    return runExample(
      script,
      binary,
      options.expected_digest ?? digest(binary),
      options.expected_version ?? "tapid 1.2.3",
      options,
    );
  });
}
test(
  "literal documentation commands execute and retain receipts",
  { skip: process.platform === "win32" },
  async () => {
    const report = await fixtureRun("mkdir demo\ncd demo\ntapid init\n");
    assert.equal(report.status, "passed");
    assert.deepEqual(
      report.commands.map((item) => item.command),
      ["mkdir demo", "cd demo", "tapid init"],
    );
  },
);

const unix = { skip: process.platform === "win32" };
const powershell = {
  skip: process.platform === "win32" || !hasCommand("pwsh"),
};
const fixture = (name: string, check: () => Promise<void>) =>
  test(name, unix, check);
async function publishedFixture(
  tag: string,
  example: string,
  body = "exit 0",
  extra: string[] = [],
) {
  return temporary((path) => {
    const binary = join(path, "tapid"),
      report = join(path, "report.json");
    write(
      binary,
      `#!/bin/sh\nif [ "$1" = --version ]; then echo "tapid ${tag.slice(1)}"; exit; fi\n${body}\n`,
    );
    chmodSync(binary, 0o755);
    const result = spawnSync(
      process.execPath,
      nodeArgs("scripts/check-doc-examples.ts", [
        "--lane",
        "published",
        "--example",
        example,
        "--binary",
        binary,
        "--expected-sha256",
        digest(binary),
        "--expected-version",
        "tapid " + tag.slice(1),
        "--release-tag",
        tag,
        "--allow-network",
        "--report",
        report,
        ...extra,
      ]),
      { encoding: "utf8", timeout: 10_000 },
    );
    assert.ifError(result.error);
    return { code: result.status, report: json(report) };
  });
}
const inventory = json(join(root, "docs/examples/contracts.json"));
const upgradeCapability = inventory.capabilities.find(c => c.id === "self-upgrade");
const reviewedTags: string[] = upgradeCapability.expected_releases;
const knownTags: string[] = [...new Set<string>(inventory.examples.flatMap(example =>
  Object.keys(example.release_expectations ?? {}),
))];
const unknownMajor = knownTags.reduce((maximum, tag) => {
  const major = BigInt(tag.slice(1).split(".")[0]);
  return major > maximum ? major : maximum;
}, 0n) + 1n;
const unknownTag = `v${unknownMajor}.0.0`;

test("upgrade inventory agrees on reviewed supported releases", () => {
  assert.ok(reviewedTags.length > 0);
  assert.equal(upgradeCapability.first_supported_release, "v0.0.10");
  for (const id of ["upgrade", "upgrade-help"]) {
    const expectations = inventory.examples.find(example => example.id === id).release_expectations;
    const supportedTags = Object.entries(expectations)
      .filter(([, outcome]: [string, any]) => outcome.exit_code === 0 && !outcome.skip)
      .map(([tag]) => tag);
    assert.deepEqual(supportedTags.sort(), [...reviewedTags].sort());
  }
});
for (const tag of reviewedTags)
  fixture(`${tag} upgrade help has reviewed expectation`, async () => {
    const { code, report } = await publishedFixture(tag, "upgrade-help");
    assert.equal(code, 0, JSON.stringify(report));
    assert.equal(report.status, "passed");
  });
fixture(
  "published upgrade skips only reviewed unsupported release",
  async () => {
    const { code, report } = await publishedFixture(
      "v0.0.9",
      "upgrade",
      "exit 99",
    );
    assert.equal(code, 0);
    assert.equal(report.status, "skipped");
    assert.equal(report.examples[0].status, "skipped");
    assert.deepEqual(report.examples[0].commands, []);
    assert.match(report.examples[0].reason, /no upgrade subcommand/);
  },
);
fixture("published upgrade unknown tags require review", async () => {
  for (const example of ["upgrade", "upgrade-help"]) {
    const { code, report } = await publishedFixture(unknownTag, example);
    assert.notEqual(code, 0);
    assert.match(report.error, /needs review/);
    assert.deepEqual(report.examples, []);
  }
});
fixture(
  "published upgrade supported releases require destination",
  async () => {
    for (const tag of reviewedTags) {
      const { code, report } = await publishedFixture(tag, "upgrade");
      assert.notEqual(code, 0);
      assert.match(report.examples[0].error, /explicit expected target/);
    }
  },
);
for (const tag of reviewedTags)
  fixture(
    `published ${tag} upgrade checks exact destination and state`,
    async () => {
      const sourceVersion = tag.slice(1);
      const [major, minor, patch] = sourceVersion.split(".");
      const wrongVersion = `tapid ${major}.${minor}.${BigInt(patch) + 1n}`;
      const replacement = `#!/bin/sh\necho "tapid ${sourceVersion}"\n`;
      const state = JSON.stringify({
        schema: "tapid-release-state-v2",
        verification: "checksum",
        last_known_good: { version: sourceVersion, artifact_sha256: "a".repeat(64) },
      });
      const body = `[ "$2" = --dry-run ] && exit 0\ntest -f "$(dirname "$0")/.tapid-managed" || exit 1\nprintf %s ${quote(state)} > "$(dirname "$0")/.tapid-release-state.json"\nprintf %s ${quote(replacement)} > "$0"`;
      for (const [hash, version, error] of [
        [sha256(replacement), `tapid ${sourceVersion}`, null],
        ["0".repeat(64), `tapid ${sourceVersion}`, "upgrade target digest mismatch"],
        [sha256(replacement), wrongVersion, "upgrade target version mismatch"],
      ]) {
        const { code, report } = await publishedFixture(
          tag,
          "upgrade",
          body,
          [
            "--upgrade-target-sha256",
            hash!,
            "--upgrade-target-version",
            version!,
          ],
        );
        if (error) {
          assert.notEqual(code, 0);
          assert.equal(report.examples[0].error, error);
        } else {
          assert.equal(code, 0, JSON.stringify(report));
          assert.deepEqual(report.examples[0].binary_after, {
            sha256: hash,
            version,
          });
          assert.equal(report.examples[0].upgrade_state.verification, "checksum");
        }
      }
    },
  );
fixture("0.0.9 help enforces negative outcome", async () => {
  assert.equal(
    (
      await publishedFixture(
        "v0.0.9",
        "upgrade-help",
        "printf \"unrecognized subcommand 'upgrade'\\n\"; exit 2",
      )
    ).code,
    0,
  );
  assert.notEqual((await publishedFixture("v0.0.9", "upgrade-help")).code, 0);
});
fixture("wrong digest rejected before commands", async () => {
  const report = await fixtureRun("tapid init\n", "exit 0", {
    expected_digest: "0".repeat(64),
  });
  assert.equal(report.status, "failed");
  assert.equal(report.failure_class, "provenance");
  assert.deepEqual(report.commands, []);
});
fixture(
  "version mismatch retains observed probe and rejects commands",
  async () => {
    const report = await fixtureRun("tapid init\n", "exit 0", {
      expected_version: "tapid 9.9.9",
    });
    assert.equal(report.status, "failed");
    assert.equal(report.failure_class, "provenance");
    assert.deepEqual(report.commands, []);
    assert.deepEqual(report.version_probe, {
      exit_code: 0,
      output: "tapid 1.2.3\n",
      failure_class: null,
    });
  },
);
fixture("changed frozen lockfile fails", async () => {
  const report = await fixtureRun(
    "tapid install\ntapid install --offline --frozen\n",
    'printf "%s" "$*" > tapid.lock',
  );
  assert.equal(report.status, "failed");
  assert.equal(report.failure_class, "assertion");
});
fixture(
  "path substitution and shell syntax rejected before execution",
  async () => {
    for (const command of [
      "PATH=/bin tapid init",
      "cd ../escape",
      "cd demo extra",
      "tapid init; true",
      "/bin/true",
      "mkdir -p /tmp/escape",
    ]) {
      const report = await fixtureRun("mkdir demo\n" + command + "\n");
      assert.equal(report.status, "failed");
      assert.equal(report.failure_class, "contract");
      assert.deepEqual(report.commands, []);
    }
  },
);
fixture("process timeout is bounded failure", async () => {
  const report = await fixtureRun("tapid init\n", "sleep 10", { timeout: 0.5 });
  assert.equal(report.status, "failed");
  assert.equal(report.failure_class, "timeout");
  assert.equal(report.commands.at(-1)!.exit_code, null);
});
for (const failure of ["timeout", "output-limit"]) {
  test(
    `process ${failure} signals its process group only once`,
    unix,
    async (t) => {
      const originalKill = process.kill.bind(process);
      const signals = new Map<number, number>();
      t.mock.method(
        process,
        "kill",
        (pid: number, signal?: NodeJS.Signals | number) => {
          assert.ok(pid < 0, "termination must target the detached process group");
          const count = (signals.get(pid) ?? 0) + 1;
          signals.set(pid, count);
          if (count > 1) {
            throw Object.assign(new Error("kill EPERM"), { code: "EPERM" });
          }
          return originalKill(pid, signal);
        },
      );
      const report = await fixtureRun(
        "tapid init\n",
        failure === "timeout"
          ? "sleep 10"
          : "while :; do printf abcdefghijklmnopqrstuvwxyz; done",
        failure === "timeout" ? { timeout: 0.1 } : { output_limit: 128 },
      );
      assert.equal(report.status, "failed");
      assert.equal(report.failure_class, failure);
      assert.equal(report.commands.at(-1)!.exit_code, null);
      assert.ok(signals.size > 0);
      for (const count of signals.values()) assert.equal(count, 1);
    },
  );
}
test("canonical quickstart keeps existing directory prerequisite", () => {
  assert.equal(
    read(join(root, "docs/examples/quickstart.sh")),
    "mkdir demo\ncd demo\ntapid init\ntapid i is-char\ntapid install --offline --frozen\n",
  );
});
fixture("binary replacement is not silent success", async () => {
  const report = await fixtureRun("tapid init\n", 'printf "changed" > "$0"');
  assert.equal(report.status, "failed");
  assert.equal(report.failure_class, "provenance");
});
test("Cargo artifact must be unique inside dedicated target", async () =>
  temporary((path) => {
    const binary = join(path, "tapid");
    write(binary, "artifact");
    const artifact = {
      reason: "compiler-artifact",
      target: { name: "tapid", kind: ["bin"] },
      executable: binary,
    };
    assert.equal(
      cargoExecutable(JSON.stringify(artifact), path),
      realpathSync(binary),
    );
    assert.throws(() =>
      cargoExecutable(
        JSON.stringify({
          ...artifact,
          executable: join(path, "../unrelated/tapid"),
        }),
        path,
      ),
    );
    assert.throws(() => cargoExecutable("", path));
    const second = join(path, "second");
    write(second, "artifact");
    assert.throws(() =>
      cargoExecutable(
        JSON.stringify(artifact) +
          "\n" +
          JSON.stringify({ ...artifact, executable: second }),
        path,
      ),
    );
  }));
fixture(
  "missing manifest assertion fails even when command exits zero",
  async () => {
    const report = await fixtureRun("tapid init\n", "exit 0", {
      assertions: ["manifest"],
    });
    assert.equal(report.status, "failed");
    assert.equal(report.failure_class, "assertion");
  },
);
fixture("empty example cannot pass", async () => {
  const report = await fixtureRun("# nothing executed\n");
  assert.equal(report.status, "failed");
  assert.equal(report.failure_class, "contract");
});
test("I/O error returns report instead of losing evidence", async () =>
  temporary(async (path) => {
    const report = await runExample(
      join(path, "missing-example"),
      join(path, "missing-binary"),
      "0".repeat(64),
      "tapid 1.2.3",
    );
    assert.equal(report.status, "failed");
    assert.ok(report.error);
  }));
fixture("oversized example rejected before execution", async () => {
  const report = await fixtureRun("#" + "x".repeat(32768) + "\ntapid init\n");
  assert.equal(report.status, "failed");
  assert.deepEqual(report.commands, []);
});
test("CLI unknown example emits failed report", async () =>
  temporary((path) => {
    const report = join(path, "report.json");
    const result = spawnSync(
      process.execPath,
      nodeArgs("scripts/check-doc-examples.ts", [
        "--lane",
        "source",
        "--example",
        "unknown",
        "--report",
        report,
      ]),
      { timeout: 10_000 },
    );
    assert.ifError(result.error);
    assert.notEqual(result.status, 0);
    assert.equal(json(report).status, "failed");
  }));
fixture("explicit upgrade target allows only expected transition", async () => {
  const replacement = '#!/bin/sh\necho "tapid 2.0.0"\n';
  const report = await fixtureRun(
    "tapid upgrade\ntapid --version\n",
    `test -f "$(dirname "$0")/.tapid-managed" || exit 1\nprintf %s ${quote(replacement)} > "$0"`,
    { upgrade_target: { sha256: sha256(replacement), version: "tapid 2.0.0" } },
  );
  assert.equal(report.status, "passed", JSON.stringify(report));
  assert.equal(report.binary_after.version, "tapid 2.0.0");
});
fixture("upgrade provenance requires persisted state", async () => {
  const report = await fixtureRun("tapid init\n", "exit 0", {
    assertions: ["upgrade-provenance"],
  });
  assert.equal(report.status, "failed");
  assert.equal(report.failure_class, "assertion");
});
test("public installer checks cannot access or push private website", () => {
  const ci = read(join(root, ".github/workflows/ci.yml")),
    workflow = ci.slice(
      ci.indexOf("\n  release-contract:"),
      ci.indexOf("\n  security:"),
    );
  assert.ok(
    !existsSync(join(root, ".github/workflows/website-installer-sync.yml")),
  );
  for (const forbidden of [
    "repository: LimeTip/tapid-web",
    "TAPID_WEB_SYNC_TOKEN",
    "git push",
    "sync-website:",
    "continue-on-error:",
  ])
    assert.ok(!workflow.includes(forbidden));
  for (const required of [
    "sh -n scripts/install.sh",
    "sh scripts/install.sh --help",
    "[System.Management.Automation.PSParser]::Tokenize(",
    "[ref]$errors",
    "exit 1",
  ])
    assert.ok(workflow.includes(required));
});
test("required CI runs offline contract and preserves live separation", () => {
  const ci = read(join(root, ".github/workflows/ci.yml"));
  assert.ok(
    ci.includes(
      "node --experimental-strip-types --test tests/doc_examples_test.ts",
    ),
  );
  assert.ok(ci.includes("--lane source --example init --example upgrade-help"));
  assert.ok(!ci.includes("--allow-network"));
  assert.ok(
    ci.includes(
      "node --experimental-strip-types scripts/check-release-record.ts --binary target/debug/tapid",
    ),
  );
  const integration = ci.indexOf("Verify generated release record end to end");
  assert.ok(
    integration > ci.indexOf("cargo test --workspace --all-features --locked"),
  );
  assert.ok(ci.slice(integration).includes("cargo build --locked --bin tapid"));
});
test("public smoke uses published binary rather than source build", () => {
  const workflow = read(
    join(root, ".github/workflows/release-public-smoke.yml"),
  );
  assert.ok(workflow.includes("--lane published"));
  assert.ok(workflow.includes("--example quickstart"));
  assert.ok(!workflow.includes("cargo build"));
  const templateLiterals = new Set(
    Array.from(workflow.matchAll(/`([^`\r\n]*)`/g), ([, value]) => value),
  );
  assert.ok(
    templateLiterals.has(
      "https://github.com/LimeTip/tapid/releases/download/${tag}/install.${extension}",
    ),
  );
  for (const extension of ["sh", "ps1"]) {
    assert.ok(workflow.includes("https://tapid.dev/install." + extension));
    assert.ok(workflow.includes("selected_installer_" + extension));
    assert.ok(workflow.includes("latest_installer_" + extension));
  }
  assert.ok(
    workflow.includes(
      "Check previous-version upgrade and repeat upgrade through the public service",
    ),
  );
  assert.ok(workflow.includes("is already up to date"));
});
test("public smoke reuses native capability validator", () => {
  const workflow = read(
    join(root, ".github/workflows/release-public-smoke.yml"),
  );
  assert.equal(
    workflow.split("node tests/fixtures/validate_consumer_project.js --binary")
      .length - 1,
    2,
  );
  for (const required of [
    '--binary "$binary" --release-tag "$RELEASE_TAG"',
    "--binary $binary --release-tag $env:RELEASE_TAG",
  ])
    assert.ok(workflow.includes(required));
  for (const forbidden of ["test -- forwarded 0", "test -- wrong 0"])
    assert.ok(!workflow.includes(forbidden));
});
test("public smoke independent checks use explicit prerequisites", () => {
  const workflow = read(
    join(root, ".github/workflows/release-public-smoke.yml"),
  );
  for (const job of [
    workflow.split("  unix:")[1].split("  windows:")[0],
    workflow.split("  windows:")[1],
  ]) {
    const latest = job
      .split("      - name: Install latest release through discovery")[1]
      .split("      - name:")[0];
    assert.ok(
      latest.includes(
        "if: ${{ !cancelled() && steps.install_public.outcome == 'success' }}",
      ),
    );
    assert.ok(latest.includes("id: install_latest"));
    assert.ok(job.includes("id: install_published"));
  }
  const upgrade = workflow
    .split("      - name: Run canonical published upgrade")[1]
    .split("      - name:")[0];
  assert.ok(
    upgrade.includes(
      "if: ${{ !cancelled() && steps.install_published.outcome == 'success' && steps.install_latest.outcome == 'success' }}",
    ),
  );
  assert.ok(upgrade.includes("id: upgrade"));
  const evidence = workflow
    .split("      - name: Retain published upgrade evidence")[1]
    .split("  windows:")[0];
  assert.ok(
    evidence.includes(
      "if: ${{ always() && steps.upgrade.outcome != 'skipped' }}",
    ),
  );
  assert.ok(evidence.includes("if-no-files-found: error"));
  assert.ok(!workflow.includes("continue-on-error:"));
});
test("native capability validator regressions", () => {
  const result = spawnSync(
    process.execPath,
    ["--test", join(root, "tests/fixtures/validate_consumer_project.test.js")],
    { encoding: "utf8", timeout: 30_000 },
  );
  assert.ifError(result.error);
  assert.equal(result.status, 0, result.stdout + result.stderr);
});
async function powershellFixture(
  body: string,
  args: string[],
  check: (evidence: any, result: any, path: string) => void,
) {
  return temporary((path) => {
    const binary = join(path, "tapid.exe"),
      report = join(path, "report.json");
    write(binary, "#!/bin/sh\n" + body + "\n");
    chmodSync(binary, 0o755);
    const hashArgs = args.includes("-ExpectedSha256")
      ? []
      : ["-ExpectedSha256", digest(binary)];
    const result = spawnSync(
      "pwsh",
      [
        "-NoProfile",
        "-File",
        join(root, "scripts/check-doc-examples.ps1"),
        "-Binary",
        binary,
        ...hashArgs,
        "-ExpectedVersion",
        "tapid 1.2.3",
        "-ReleaseTag",
        "v1.2.3",
        "-ReportPath",
        report,
        ...args,
      ],
      { encoding: "utf8", timeout: 30_000 },
    );
    assert.ifError(result.error);
    check(json(report), result, path);
  });
}
test(
  "native PowerShell requires network opt-in before execution",
  powershell,
  async () =>
    temporary(async (path) => {
      const marker = join(path, "executed");
      await powershellFixture(
        `touch ${quote(marker)}\necho "tapid 1.2.3"`,
        [],
        (evidence, result) => {
          assert.notEqual(result.status, 0);
          assert.equal(evidence.status, "failed");
          assert.match(evidence.error, /-AllowNetwork/);
          assert.deepEqual(evidence.commands, []);
          assert.ok(!existsSync(marker));
        },
      );
    }),
);
test(
  "native PowerShell quickstart executes maintained file",
  powershell,
  async () =>
    powershellFixture(
      `if [ "$1" = --version ]; then echo "tapid 1.2.3"; exit; fi
if [ "$1" = init ]; then printf '{"dependencies":{"is-char":"1"}}' > package.json; fi
if [ "$1" = i ]; then mkdir -p node_modules/is-char; printf '{"name":"is-char"}' > node_modules/is-char/package.json; printf lock > tapid.lock; fi`,
      ["-AllowNetwork"],
      (evidence, result) => {
        assert.equal(result.status, 0, result.stderr);
        assert.equal(evidence.status, "passed");
        assert.equal(evidence.commands.length, 5);
      },
    ),
);
test(
  "native PowerShell allocation collisions preserve existing data",
  powershell,
  async () => {
    const source = read(join(root, "scripts/check-doc-examples.ps1")),
      rootLine = source
        .split("\n")
        .find((line) => line.startsWith("$root = "))!;
    for (const kind of ["directory", "file"])
      await temporary((path) => {
        const collision = join(path, "collision");
        if (kind === "directory") mkdirSync(collision);
        const sentinel =
          kind === "directory" ? join(collision, "keep.txt") : collision;
        write(sentinel, "pre-existing data");
        const script = join(path, "scripts/check-doc-examples.ps1");
        write(
          script,
          source.replace(
            rootLine,
            "$root = '" + collision.replaceAll("'", "''") + "'",
          ),
        );
        write(
          join(path, "docs/examples/quickstart.ps1"),
          read(join(root, "docs/examples/quickstart.ps1")),
        );
        const binary = join(path, "unused-binary"),
          report = join(path, "report.json");
        write(binary, "allocation must fail before execution");
        const result = spawnSync(
          "pwsh",
          [
            "-NoProfile",
            "-File",
            script,
            "-AllowNetwork",
            "-Binary",
            binary,
            "-ExpectedSha256",
            digest(binary),
            "-ExpectedVersion",
            "tapid 1.2.3",
            "-ReleaseTag",
            "v1.2.3",
            "-ReportPath",
            report,
          ],
          { encoding: "utf8", timeout: 30_000 },
        );
        assert.ifError(result.error);
        assert.notEqual(result.status, 0);
        const evidence = json(report);
        assert.equal(evidence.status, "failed");
        assert.equal(evidence.failure_class, "execution");
        assert.ok(evidence.error.includes(collision));
        assert.deepEqual(evidence.commands, []);
        assert.equal(read(sentinel), "pre-existing data");
        if (kind === "directory")
          assert.deepEqual(readdirSync(collision), ["keep.txt"]);
      });
  },
);
test(
  "native PowerShell version mismatch retains observed probe",
  powershell,
  async () => {
    for (const code of [0, 7])
      await powershellFixture(
        `echo "tapid 9.9.9"\nexit ${code}`,
        ["-AllowNetwork"],
        (evidence, result) => {
          assert.notEqual(result.status, 0);
          assert.equal(evidence.status, "failed");
          assert.equal(evidence.failure_class, "provenance");
          assert.equal(evidence.error, "binary version mismatch");
          assert.deepEqual(evidence.commands, []);
          assert.deepEqual(evidence.version_probe, {
            exit_code: code,
            output: "tapid 9.9.9\n",
          });
        },
      );
  },
);
test(
  "native PowerShell wrong digest persists failure evidence",
  powershell,
  async () =>
    powershellFixture(
      'echo "tapid 1.2.3"',
      ["-ExpectedSha256", "0".repeat(64), "-AllowNetwork"],
      (evidence, result) => {
        assert.notEqual(result.status, 0);
        assert.equal(evidence.failure_class, "provenance");
      },
    ),
);
fixture("released upgrade absence is explicit negative contract", async () => {
  const report = await fixtureRun(
    "tapid upgrade --help\n",
    "printf \"error: unrecognized subcommand 'upgrade'\\n\"; exit 2",
    { expected_exit: 2, expected_output: "unrecognized subcommand 'upgrade'" },
  );
  assert.equal(report.status, "passed");
  assert.equal(report.commands[0].exit_code, 2);
});
fixture("failed cd stops without implicit directory creation", async () => {
  const report = await fixtureRun("cd demo\ntapid init\n");
  assert.equal(report.status, "failed");
  assert.equal(report.commands.length, 1);
  assert.equal(report.commands[0].exit_code, 1);
});
fixture("missing mkdir does not get inserted", async () => {
  assert.equal(
    (await fixtureRun("tapid init demo\n", '[ -d "$2" ] || exit 1')).status,
    "failed",
  );
});
fixture("output limit stops noisy child", async () => {
  const report = await fixtureRun(
    "tapid init\n",
    "while :; do printf abcdefghijklmnopqrstuvwxyz; done",
    { output_limit: 128 },
  );
  assert.equal(report.status, "failed");
  assert.equal(report.failure_class, "output-limit");
  assert.ok(Buffer.byteLength(report.commands[0].output) <= 128);
});
fixture("success records each literal command", async () => {
  const report = await fixtureRun(
    "mkdir demo\ntapid init demo\ncd demo\n",
    '[ -d "$2" ] || exit 1\nprintf \'{"name":"demo"}\\n\' > "$2/package.json"',
  );
  assert.equal(report.status, "passed");
  assert.deepEqual(
    report.commands.map((c) => c.command),
    ["mkdir demo", "tapid init demo", "cd demo"],
  );
  assert.deepEqual(
    report.commands.map((c) => c.exit_code),
    [0, 0, 0],
  );
});
fixture("timeouts kill descendants before they can write later", async () =>
  temporary(async (path) => {
    const marker = join(path, "survived");
    const report = await fixtureRun(
      "tapid init\n",
      `(sleep 1; printf survived > ${quote(marker)}) &\nwait`,
      { timeout: 0.2 },
    );
    assert.equal(report.failure_class, "timeout");
    await new Promise((resolve) => setTimeout(resolve, 1200));
    assert.ok(!existsSync(marker), "descendant outlived the bounded process");
  }),
);
