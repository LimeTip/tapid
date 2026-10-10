import { strict as assert } from "node:assert";
import { test } from "node:test";
import { join } from "node:path";
import { spawnSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdirSync, realpathSync } from "node:fs";
import { compare } from "../scripts/compare-news-site-package-graphs.ts";
import {
  root,
  json,
  temporary,
  writeJson,
  fixtureArchive,
  nodeArgs,
} from "./script-support.ts";
const project = join(root, "examples/news-site-consumer");
const manifest = json(join(project, "package.json")),
  lock = json(join(project, "package-lock.json"));
function assertPinnedGraph(manifest: any, lock: any) {
  assert.equal(lock.lockfileVersion, 3);
  const records = lock.packages,
    overrides = manifest.overrides ?? {};
  for (const [name, version] of Object.entries(overrides))
    assert.equal(version, records["node_modules/" + name].version, name);
  for (const kind of ["dependencies", "devDependencies"]) {
    assert.deepEqual(manifest[kind], records[""][kind]);
    for (const [name, requirement] of Object.entries(manifest[kind]))
      assert.equal(requirement, records["node_modules/" + name].version, name);
  }
  for (const [parent, record] of Object.entries(records) as [string, any][]) {
    if (!parent) continue;
    assert.ok(
      !parent.replace(/^node_modules\//, "").includes("/node_modules/"),
    );
    for (const kind of ["dependencies", "optionalDependencies"])
      for (const [name, requirement] of Object.entries(record[kind] ?? {})) {
        const provider = records["node_modules/" + name].version,
          effective = overrides[name] ?? requirement;
        assert.equal(
          effective,
          provider,
          `${parent} -> ${name}: floating native requirement ${effective}; pin the fixture override to npm's locked ${provider}`,
        );
      }
  }
}
test("caniuse native requirement cannot select newer registry version", () =>
  assert.equal(
    manifest.overrides["caniuse-lite"] ??
      lock.packages["node_modules/next"].dependencies["caniuse-lite"],
    lock.packages["node_modules/caniuse-lite"].version,
  ));
test("native resolution inputs pin entire npm reference graph", () =>
  assertPinnedGraph(manifest, lock));
for (const [name, label] of [
  ["caniuse-lite", "caniuse"],
  ["sharp", "optional sharp"],
])
  test(`missing ${label} pin is rejected`, () => {
    const changed = structuredClone(manifest);
    delete changed.overrides[name];
    assert.throws(
      () => assertPinnedGraph(changed, lock),
      new RegExp("next -> " + name),
    );
  });
test("override and committed lock cannot disagree", () => {
  const changed = structuredClone(manifest);
  changed.overrides["caniuse-lite"] = "1.0.30001816";
  assert.throws(() => assertPinnedGraph(changed, lock), /caniuse-lite/);
});
test(
  "native resolver replays publication drift then honors fixture pin",
  { skip: !process.env.TAPID_NEWS_FIXTURE_BINARY && !process.env.CI },
  async () =>
    temporary((path) => {
      assert.ok(
        process.env.TAPID_NEWS_FIXTURE_BINARY,
        "TAPID_NEWS_FIXTURE_BINARY is required in news-site CI",
      );
      const binary = realpathSync(process.env.TAPID_NEWS_FIXTURE_BINARY!);
      const requirement =
        lock.packages["node_modules/next"].dependencies["caniuse-lite"];
      const packages = [
        ["next", "15.5.27", { "caniuse-lite": requirement }],
        ["caniuse-lite", "1.0.30001815", {}],
        ["caniuse-lite", "1.0.30001816", {}],
      ].map(([name, version, dependencies]) => {
        const metadata = { name, version, dependencies },
          artifact = fixtureArchive(
            "package/package.json",
            Buffer.from(JSON.stringify(metadata)),
          );
        return {
          ...metadata,
          registry: "https://registry.npmjs.org",
          integrity:
            "sha512-" + createHash("sha512").update(artifact).digest("base64"),
          artifact: "base64:" + artifact.toString("base64"),
        };
      });
      const registry = join(path, "registry.json");
      writeJson(registry, { packages });
      const home = join(path, "home");
      mkdirSync(home);
      for (const [label, overrides, expected] of [
        ["floating", {}, "1.0.30001816"],
        [
          "pinned",
          { "caniuse-lite": manifest.overrides["caniuse-lite"] ?? requirement },
          "1.0.30001815",
        ],
      ] as const) {
        const project = join(path, label);
        mkdirSync(project);
        writeJson(join(project, "package.json"), {
          name: "fixture",
          version: "1.0.0",
          dependencies: { next: "15.5.27" },
          overrides,
        });
        writeJson(join(project, "package-lock.json"), lock);
        const result = spawnSync(
          binary,
          [
            "install",
            "--project-dir",
            project,
            "--registry-fixture",
            registry,
            "--store-dir",
            join(path, "store"),
          ],
          {
            env: {
              PATH: process.env.PATH,
              HOME: home,
              TMPDIR: path,
              SystemRoot: process.env.SystemRoot,
            },
            encoding: "utf8",
            timeout: 30_000,
          },
        );
        assert.ifError(result.error);
        assert.equal(result.status, 0, result.stderr);
        assert.equal(
          json(join(project, "node_modules/caniuse-lite/package.json")).version,
          expected,
        );
      }
    }),
);
test("strict comparator rejects caniuse publication drift", async () =>
  temporary((path) => {
    const roots = ["npm", "tapid"].map((manager) => join(path, manager));
    roots.forEach((project, index) => {
      const rootManifest = {
        name: "fixture",
        version: "1.0.0",
        dependencies: { next: "15.5.27" },
      };
      writeJson(join(project, "package.json"), rootManifest);
      for (const [name, metadata] of Object.entries({
        next: {
          name: "next",
          version: "15.5.27",
          dependencies: { "caniuse-lite": "^1.0.30001579" },
        },
        "caniuse-lite": {
          name: "caniuse-lite",
          version: index ? "1.0.30001815" : "1.0.30001814",
        },
      }))
        writeJson(
          join(project, "node_modules", name, "package.json"),
          metadata,
        );
      writeJson(join(project, "package-lock.json"), {
        lockfileVersion: 3,
        packages: {
          "": rootManifest,
          "node_modules/next": {
            version: "15.5.27",
            resolved: "https://registry.npmjs.org/next/-/next-15.5.27.tgz",
            integrity: "sha512-test",
          },
        },
      });
      writeJson(join(project, "tapid.lock"), {
        packages: {
          next: {
            name: "next",
            version: "15.5.27",
            registry: "https://registry.npmjs.org",
            artifactIntegrity: "sha512-test",
          },
        },
      });
    });
    const report = join(path, "graph.json");
    const result = spawnSync(
      process.execPath,
      nodeArgs("scripts/compare-news-site-package-graphs.ts", [
        "--npm-root",
        roots[0],
        "--tapid-root",
        roots[1],
        "--json",
        report,
      ]),
      { encoding: "utf8", timeout: 20_000 },
    );
    assert.ifError(result.error);
    assert.equal(result.status, 1, result.stderr);
    const graph = json(report);
    for (const [manager, version] of [
      ["npmOnly", "1.0.30001814"],
      ["tapidOnly", "1.0.30001815"],
    ]) {
      assert.deepEqual(graph.reachablePackageDifferences[manager], [
        { name: "caniuse-lite", version },
      ]);
      const edges = graph.dependencyEdgeDifferences[manager];
      assert.equal(edges.length, 1);
      assert.equal(edges[0].dependency, "caniuse-lite");
      assert.equal(edges[0].provider.version, version);
    }
    assert.deepEqual(graph.sourceIdentityMismatches, []);
    assert.deepEqual(graph.integrityMismatches, []);
  }));

async function matchingGraphs(check: (npm: string, tapid: string) => void) {
  return temporary((path) => {
    const roots = ["npm", "tapid"].map((name) => join(path, name));
    for (const project of roots) {
      const manifest = {
        name: "fixture",
        version: "1.0.0",
        dependencies: { app: "1.0.0" },
        devDependencies: { peer: "2.0.0" },
        optionalDependencies: { platform: "1.0.0", absent: "1.0.0" },
      };
      writeJson(join(project, "package.json"), manifest);
      const installed = {
        app: {
          name: "app",
          version: "1.0.0",
          dependencies: { child: "^2.0.0" },
          peerDependencies: { peer: "^2.0.0", absentPeer: "*" },
          peerDependenciesMeta: { absentPeer: { optional: true } },
        },
        peer: { name: "peer", version: "2.0.0" },
        platform: { name: "platform", version: "1.0.0" },
        child: { name: "child", version: "1.0.0" },
        "app/node_modules/child": { name: "child", version: "2.0.0" },
        unused: { name: "unused", version: "9.0.0" },
      };
      const records: any = { "": manifest },
        packages: any = {};
      for (const [location, metadata] of Object.entries(installed)) {
        writeJson(
          join(project, "node_modules", location, "package.json"),
          metadata,
        );
        records["node_modules/" + location] = {
          version: metadata.version,
          resolved: `https://registry.npmjs.org/${metadata.name}/file.tgz`,
          integrity: "sha512-fixture",
          ...(metadata.name === "platform"
            ? { optional: true, os: ["darwin"] }
            : {}),
        };
        packages[location] = {
          ...metadata,
          registry: "https://registry.npmjs.org",
          artifactIntegrity: "sha512-fixture",
          ...(metadata.name === "platform"
            ? { platformContext: "os=darwin;cpu=;libc=" }
            : {}),
        };
      }
      writeJson(join(project, "package-lock.json"), {
        lockfileVersion: 3,
        packages: records,
      });
      writeJson(join(project, "tapid.lock"), { packages });
    }
    check(roots[0], roots[1]);
  });
}
test("matching nested graphs preserve peers, optional selections and unreachable packages", async () =>
  matchingGraphs((npm, tapid) => {
    const { report, code } = compare(npm, tapid);
    assert.equal(code, 0);
    assert.equal(report.counts.npmPhysicalPackages, 6);
    assert.equal(report.counts.npmReachablePackages, 4);
    assert.deepEqual(report.unreachablePhysicalPackages.npm, [
      { name: "child", version: "1.0.0" },
      { name: "unused", version: "9.0.0" },
    ]);
    assert.deepEqual(report.graphSnapshot.platformOptionalPackages.npm, [
      { name: "platform", version: "1.0.0" },
    ]);
    assert.equal(report.graphSnapshot.peerEdges.npm.length, 2);
    assert.deepEqual(report.missingRequiredEdges, { npm: [], tapid: [] });
    const child = report.graphSnapshot.dependencyEdges.npm.find(
      (edge) => edge.parent.name === "app" && edge.dependency === "child",
    );
    assert.deepEqual(child!.provider, { name: "child", version: "2.0.0" });
  }));
for (const [field, value, result] of [
  ["registry", "https://other.example", "sourceIdentityMismatches"],
  ["artifactIntegrity", "sha512-other", "integrityMismatches"],
  ["artifactIntegrity", null, "missingProvenance"],
] as const)
  test(`graph comparison rejects ${result}`, async () =>
    matchingGraphs((npm, tapid) => {
      const path = join(tapid, "tapid.lock"),
        lock = json(path);
      lock.packages.app[field] = value;
      writeJson(path, lock);
      const { report, code } = compare(npm, tapid);
      assert.equal(code, 1);
      assert.equal(report[result].length, 1);
      assert.equal(report[result][0].name, "app");
    }));
