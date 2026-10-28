// Full consumer acceptance: Tapid installs; Node runs tools; workerd runs the Worker.
import { strict as assert } from "node:assert";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath, pathToFileURL } from "node:url";
import { createServer } from "node:net";
import { createRequire } from "node:module";
import { parseArgs } from "node:util";
import { boundedProcess, digest } from "./check-doc-examples.ts";
import { compare } from "./compare-consumer-package-graphs.ts";
import { assertServer, isolatedEnvironment } from "./workers-process.ts";
const root = dirname(dirname(fileURLToPath(import.meta.url)));
const fixture = join(root, "examples/workers-consumer");
const read = (path: string) => JSON.parse(readFileSync(path, "utf8"));
async function port(): Promise<number> {
  const server = createServer();
  return new Promise((resolvePort, reject) => {
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const p = (server.address() as any).port;
      server.close((error) => (error ? reject(error) : resolvePort(p)));
    });
  });
}
export async function checkWorkers(options: {
  binary: string;
  npmCli: string;
  artifacts: string;
}) {
  assert.equal(process.version, "v22.23.3", "use pinned Node");
  const artifacts = resolve(options.artifacts);
  mkdirSync(artifacts, { recursive: true });
  const work = mkdtempSync(join(tmpdir(), "tapid-workers-consumer-"));
  const phases: any[] = [];
  const save = (name: string, value: unknown) =>
    writeFileSync(join(artifacts, name), JSON.stringify(value, null, 2) + "\n");
  const snapshot = join(work, "fixture");
  mkdirSync(snapshot);
  for (const input of [
    "package.json",
    "package-lock.json",
    "src",
    "tsconfig.json",
    "wrangler.jsonc",
  ]) {
    cpSync(join(fixture, input), join(snapshot, input), { recursive: true });
  }
  const binaryDigest = digest(resolve(options.binary));
  const project = (name: string) => {
    const path = join(work, name);
    cpSync(snapshot, path, { recursive: true });
    return path;
  };
  const envFor = (name: string) => {
    const home = join(work, name + "-home");
    mkdirSync(home);
    return isolatedEnvironment(home);
  };
  async function command(
    label: string,
    args: string[],
    cwd: string,
    env: NodeJS.ProcessEnv,
    success = true,
  ) {
    const start = Date.now();
    const result = await boundedProcess(args, {
      cwd,
      env,
      timeout: 600,
      output_limit: 2 * 1024 * 1024,
    });
    writeFileSync(join(artifacts, label + ".log"), result.output);
    phases.push({
      label,
      args,
      elapsedMs: Date.now() - start,
      ...result,
      output: undefined,
    });
    save("phases.json", phases);
    assert.equal(
      result.failure_class,
      null,
      label + ": " + result.failure_class,
    );
    if (success)
      assert.equal(result.exit_code, 0, label + ": " + result.output);
    else assert.notEqual(result.exit_code, 0, label + " must fail");
    return result;
  }
  const npmArgs = [process.execPath, resolve(options.npmCli)];
  try {
    const npmProject = project("npm"),
      npmEnv = envFor("npm");
    const version = await command(
      "npm-version",
      [...npmArgs, "--version"],
      npmProject,
      npmEnv,
    );
    assert.equal(version.output.trim(), "11.12.1", "use pinned npm baseline");
    save("toolchain.json", {
      node: process.version,
      platform: process.platform,
      arch: process.arch,
      manifest: read(join(snapshot, "package.json")),
      tapidBinarySha256: binaryDigest,
      npmLockSha256: digest(join(snapshot, "package-lock.json")),
      npmLifecycleScripts: true,
      tapidLifecycleScripts: false,
    });
    await command(
      "tapid-version",
      [resolve(options.binary), "--version"],
      work,
      npmEnv,
    );
    await command(
      "npm-ci",
      [...npmArgs, "ci", "--no-fund"],
      npmProject,
      npmEnv,
    );
    for (const scenario of ["native", "imported"]) {
      const path = project(scenario),
        env = envFor(scenario),
        store = join(work, scenario + "-store");
      const binary = resolve(options.binary);
      const install = (extra: string[], label: string) =>
        command(
          scenario + "-" + label,
          [
            binary,
            "install",
            "--project-dir",
            path,
            "--store-dir",
            store,
            ...extra,
          ],
          path,
          env,
        );
      const inputs = ["package.json", "package-lock.json"].map(
        (file) => [file, digest(join(path, file))] as const,
      );
      const unchanged = () =>
        inputs.forEach(([file, hash]) =>
          assert.equal(digest(join(path, file)), hash, file + " changed"),
        );
      if (scenario === "native") await install([], "cold-install");
      else {
        await command(
          "imported-import",
          [binary, "import-package-lock", "./package-lock.json"],
          path,
          env,
        );
        unchanged();
        assert.equal(existsSync(join(path, "node_modules")), false);
        assert.equal(existsSync(store), false, "import touched store");
        const importedLock = read(join(path, "tapid.lock"));
        const {
          verifiedTrees: _trees,
          verifiedArtifacts: _artifacts,
          ...selections
        } = importedLock;
        await install(["--frozen"], "hydrate");
        const { verifiedTrees, verifiedArtifacts, ...hydratedSelections } =
          read(join(path, "tapid.lock"));
        assert.deepEqual(
          hydratedSelections,
          selections,
          "hydration changed more than receipts",
        );
      }
      const stable = digest(join(path, "tapid.lock"));
      unchanged();
      cpSync(
        join(path, "tapid.lock"),
        join(artifacts, scenario + "-tapid.lock"),
      );
      for (const replay of ["frozen", "offline"]) {
        rmSync(join(path, "node_modules"), { recursive: true, force: true });
        assert.equal(existsSync(join(path, "node_modules")), false);
        await install(
          replay === "offline" ? ["--offline", "--frozen"] : ["--frozen"],
          replay,
        );
        unchanged();
        assert.equal(
          digest(join(path, "tapid.lock")),
          stable,
          "replay changed lock",
        );
        const result = compare(npmProject, path);
        save(scenario + "-" + replay + "-comparison.json", result.report);
        writeFileSync(
          join(artifacts, scenario + "-" + replay + "-comparison.txt"),
          result.text,
        );
        assert.equal(
          result.code,
          0,
          scenario + " graph mismatch; see comparison report",
        );
        const label = scenario + "-" + replay;
        await command(
          label + "-typecheck",
          [process.execPath, "node_modules/typescript/bin/tsc", "--noEmit"],
          path,
          env,
        );
        const wrangler = join(path, "node_modules/wrangler/bin/wrangler.js");
        // Resolve the package's own optional binaries. No external binary override,
        // dependency hook, npm-produced output or fallback download is supplied.
        const require = createRequire(wrangler);
        const workerd = require.resolve("workerd/bin/workerd");
        const workerdRequire = createRequire(workerd);
        const workerdBinary = workerdRequire.resolve(
          "@cloudflare/workerd-" +
            process.platform +
            "-" +
            (process.arch === "x64" ? "64" : process.arch) +
            "/bin/workerd",
        );
        const esbuildRequire = createRequire(require.resolve("esbuild"));
        const esbuildBinary = esbuildRequire.resolve(
          "@esbuild/" + process.platform + "-" + process.arch + "/bin/esbuild",
        );
        await command(
          label + "-packaged-workerd",
          [workerdBinary, "--version"],
          path,
          env,
        );
        await command(
          label + "-packaged-esbuild",
          [esbuildBinary, "--version"],
          path,
          env,
        );
        save(label + "-binary-hashes.json", {
          workerd: digest(workerdBinary),
          esbuild: digest(esbuildBinary),
        });
        await command(
          label + "-workerd-version",
          [process.execPath, workerd, "--version"],
          path,
          env,
        );
        await command(
          label + "-wrangler-version",
          [process.execPath, wrangler, "--version"],
          path,
          env,
        );
        await command(
          label + "-bundle",
          [
            process.execPath,
            wrangler,
            "deploy",
            "--dry-run",
            "--outdir",
            "dist",
          ],
          path,
          env,
        );
        const p = await port();
        const response = await assertServer(
          [
            process.execPath,
            wrangler,
            "dev",
            "--local",
            "--ip",
            "127.0.0.1",
            "--port",
            String(p),
            "--inspector-port",
            "0",
          ],
          {
            cwd: path,
            env,
            url: `http://127.0.0.1:${p}/`,
            expected: "TAPID_WORKERS_ACCEPTANCE_V1",
            log: join(artifacts, label + "-runtime.log"),
          },
        );
        save(label + "-response.json", response);
      }
      const negative = project(scenario + "-missing-store");
      cpSync(join(path, "tapid.lock"), join(negative, "tapid.lock"));
      const before = ["package.json", "package-lock.json", "tapid.lock"].map(
        (f) => digest(join(negative, f)),
      );
      const failure = await command(
        scenario + "-missing-store",
        [
          binary,
          "install",
          "--offline",
          "--frozen",
          "--project-dir",
          negative,
          "--store-dir",
          join(work, scenario + "-empty-store"),
        ],
        negative,
        env,
        false,
      );
      assert.match(failure.output, /store|artifact|offline|verified/i);
      assert.deepEqual(
        ["package.json", "package-lock.json", "tapid.lock"].map((f) =>
          digest(join(negative, f)),
        ),
        before,
      );
      assert.equal(existsSync(join(negative, "node_modules")), false);
      save(scenario + "-input-hashes.json", inputs);
    }
    assert.equal(
      digest(resolve(options.binary)),
      binaryDigest,
      "Tapid binary changed during validation",
    );
    save("result.json", { status: "passed", phases: phases.length });
  } catch (error) {
    save("result.json", { status: "failed", error: String(error) });
    throw error;
  } finally {
    for (const scenario of ["native", "imported"]) {
      const logs = join(work, scenario + "-home", ".wrangler", "logs");
      if (existsSync(logs))
        cpSync(logs, join(artifacts, scenario + "-wrangler-logs"), {
          recursive: true,
        });
    }
    rmSync(work, { recursive: true, force: true });
  }
}
if (
  process.argv[1] &&
  import.meta.url === pathToFileURL(resolve(process.argv[1])).href
) {
  const { values } = parseArgs({
    options: {
      binary: { type: "string" },
      "npm-cli": { type: "string" },
      artifacts: { type: "string" },
    },
  });
  if (!values.binary || !values["npm-cli"] || !values.artifacts)
    throw new Error("--binary, --npm-cli and --artifacts required");
  await checkWorkers({
    binary: values.binary,
    npmCli: values["npm-cli"],
    artifacts: values.artifacts,
  });
}
