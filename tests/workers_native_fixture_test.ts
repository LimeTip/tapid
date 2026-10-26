import { strict as assert } from "node:assert";
import { test } from "node:test";
import { spawnSync } from "node:child_process";
import { join } from "node:path";
import {
  root,
  temporary,
  json,
  writeJson,
  fixtureArchive,
} from "./script-support.ts";
import { createHash } from "node:crypto";
import { mkdirSync } from "node:fs";
test(
  "Workers native overrides survive publication drift",
  { skip: !process.env.TAPID_WORKERS_FIXTURE_BINARY && !process.env.CI },
  () =>
    temporary((path) => {
      assert.ok(
        process.env.TAPID_WORKERS_FIXTURE_BINARY,
        "source-built binary required",
      );
      const manifest = json(
        join(root, "examples/workers-consumer/package.json"),
      );
      const version = manifest.overrides["path-to-regexp"];
      const packages = [version, "6.99.0"].map((version) => {
        const metadata = { name: "path-to-regexp", version };
        const bytes = fixtureArchive(
          "package/package.json",
          Buffer.from(JSON.stringify(metadata)),
        );
        return {
          ...metadata,
          registry: "https://registry.npmjs.org",
          integrity:
            "sha512-" + createHash("sha512").update(bytes).digest("base64"),
          artifact: "base64:" + bytes.toString("base64"),
        };
      });
      writeJson(join(path, "registry.json"), {
        packages: [
          ...packages,
          ...["wrangler"].map((name) => {
            const metadata = {
              name,
              version: "4.153.0",
              dependencies: { "path-to-regexp": "^6.3.0" },
            };
            const bytes = fixtureArchive(
              "package/package.json",
              Buffer.from(JSON.stringify(metadata)),
            );
            return {
              ...metadata,
              registry: "https://registry.npmjs.org",
              integrity:
                "sha512-" + createHash("sha512").update(bytes).digest("base64"),
              artifact: "base64:" + bytes.toString("base64"),
            };
          }),
        ],
      });
      for (const [label, overrides, expected] of [
        ["floating", {}, "6.99.0"],
        ["pinned", { "path-to-regexp": version }, version],
      ] as const) {
        const project = join(path, label);
        mkdirSync(project);
        const home = join(path, label + "-home");
        mkdirSync(home);
        writeJson(join(project, "package.json"), {
          name: "fixture",
          version: "1.0.0",
          dependencies: { wrangler: "4.153.0" },
          overrides,
        });
        const result = spawnSync(
          process.env.TAPID_WORKERS_FIXTURE_BINARY!,
          [
            "install",
            "--project-dir",
            project,
            "--registry-fixture",
            join(path, "registry.json"),
            "--store-dir",
            join(path, label + "-store"),
          ],
          {
            env: {
              PATH: process.env.PATH,
              HOME: home,
              TMPDIR: path,
              SystemRoot: process.env.SystemRoot,
            },
            encoding: "utf8",
            timeout: 30000,
          },
        );
        assert.ifError(result.error);
        assert.equal(result.status, 0, result.stderr);
        const lock = json(join(project, "tapid.lock"));
        assert.ok(
          Object.values(lock.packages).some(
            (r: any) => r.name === "path-to-regexp" && r.version === expected,
          ),
        );
      }
    }),
);
