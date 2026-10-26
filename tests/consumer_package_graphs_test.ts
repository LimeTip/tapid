import { strict as assert } from "node:assert";
import { test } from "node:test";
import { join } from "node:path";
import { compare } from "../scripts/compare-consumer-package-graphs.ts";
import { temporary, writeJson, json } from "./script-support.ts";
async function graphs(body: (a: string, b: string) => void) {
  await temporary((path) => {
    const roots = ["npm", "tapid"].map((n) => join(path, n));
    for (const root of roots) {
      writeJson(join(root, "package.json"), {
        name: "app",
        version: "1.0.0",
        dependencies: { dep: "1.0.0" },
      });
      writeJson(join(root, "node_modules/dep/package.json"), {
        name: "dep",
        version: "1.0.0",
      });
      const packages = {
        "node_modules/dep": {
          version: "1.0.0",
          resolved: "https://registry.npmjs.org/dep/-/dep-1.0.0.tgz",
          integrity: "sha512-test",
        },
      };
      writeJson(join(root, "package-lock.json"), {
        lockfileVersion: 3,
        packages,
      });
      writeJson(join(root, "tapid.lock"), {
        lockfileVersion: 8,
        npmLock: { lockfileVersion: 3, packages },
        verifiedArtifacts: {
          "node_modules/dep": {
            resolved: packages["node_modules/dep"].resolved,
            integrity: "sha512-test",
            treeDigest: "sha256-test",
          },
        },
      });
    }
    body(...(roots as [string, string]));
  });
}
test("imported schema uses placement-bound provenance and ignores embedded manifests", () =>
  graphs((a, b) => {
    writeJson(join(b, "node_modules/dep/dist/package.json"), {
      name: "embedded",
      version: "2.0.0",
    });
    assert.equal(compare(a, b).code, 0);
  }));
test("same-origin artifact URL changes fail comparison", () =>
  graphs((a, b) => {
    const lock = json(join(b, "tapid.lock"));
    lock.npmLock.packages["node_modules/dep"].resolved =
      "https://registry.npmjs.org/dep/-/other.tgz";
    writeJson(join(b, "tapid.lock"), lock);
    assert.equal(compare(a, b).code, 1);
  }));
test("unknown Tapid schema fails explicitly", () =>
  graphs((a, b) => {
    writeJson(join(b, "tapid.lock"), { lockfileVersion: 99, packages: {} });
    assert.throws(() => compare(a, b), /schema/);
  }));
for (const field of ["resolved", "integrity"] as const)
  test(
    "one matching duplicate cannot conceal a second " + field + " mismatch",
    () =>
      graphs((a, b) => {
        for (const root of [a, b]) {
          const manifest = json(join(root, "package.json"));
          manifest.dependencies.other = "1.0.0";
          writeJson(join(root, "package.json"), manifest);
          writeJson(join(root, "node_modules/other/package.json"), {
            name: "other",
            version: "1.0.0",
            dependencies: { dep: "1.0.0" },
          });
          writeJson(
            join(root, "node_modules/other/node_modules/dep/package.json"),
            { name: "dep", version: "1.0.0" },
          );
          const lock = json(join(root, "package-lock.json"));
          lock.packages["node_modules/other"] = {
            version: "1.0.0",
            resolved: "https://registry.npmjs.org/other/-/other-1.0.0.tgz",
            integrity: "sha512-other",
            dependencies: { dep: "1.0.0" },
          };
          lock.packages["node_modules/other/node_modules/dep"] = {
            ...lock.packages["node_modules/dep"],
          };
          if (root === a) writeJson(join(root, "package-lock.json"), lock);
          else {
            lock.packages["node_modules/other/node_modules/dep"][field] =
              field === "integrity"
                ? "sha512-different"
                : "https://registry.npmjs.org/dep/-/different.tgz";
            const imported = json(join(root, "tapid.lock"));
            imported.npmLock = lock;
            for (const [id, r] of Object.entries(lock.packages) as [
              string,
              any,
            ][])
              imported.verifiedArtifacts[id] = {
                resolved: r.resolved,
                integrity: r.integrity,
                treeDigest: "sha256-test",
              };
            writeJson(join(root, "tapid.lock"), imported);
          }
        }
        const result = compare(a, b);
        assert.equal(result.code, 1);
        assert.ok(result.report.instanceEvidence.differences.length);
      }),
  );
test("foreign optional branch descendants are explicitly permitted omissions", () =>
  graphs((a, b) => {
    for (const root of [a, b]) {
      const file = join(root, root === a ? "package-lock.json" : "tapid.lock"),
        lock = json(file),
        npm = root === a ? lock : lock.npmLock;
      const manifest = json(join(root, "package.json"));
      manifest.optionalDependencies = { foreign: "1.0.0" };
      writeJson(join(root, "package.json"), manifest);
      npm.packages["node_modules/foreign"] = {
        version: "1.0.0",
        resolved: "https://registry.npmjs.org/foreign/-/foreign-1.0.0.tgz",
        integrity: "sha512-test",
        optional: true,
        os: ["nonexistent"],
        dependencies: { child: "1.0.0" },
      };
      npm.packages["node_modules/child"] = {
        version: "1.0.0",
        resolved: "https://registry.npmjs.org/child/-/child-1.0.0.tgz",
        integrity: "sha512-test",
        optional: true,
      };
      writeJson(file, lock);
    }
    const result = compare(a, b);
    assert.equal(result.code, 0);
    assert.equal(result.report.instanceEvidence.npm!.omitted.length, 2);
    assert.ok(
      result.report.instanceEvidence.npm!.omitted.every((r) => r.permitted),
    );
  }));
test("missing verification receipt fails closed", () =>
  graphs((a, b) => {
    const lock = json(join(b, "tapid.lock"));
    delete lock.verifiedArtifacts;
    writeJson(join(b, "tapid.lock"), lock);
    const result = compare(a, b);
    assert.equal(result.code, 1);
    assert.match(result.report.instanceEvidence.errors.join("\n"), /receipt/);
  }));
test("bundled/non-registry lock features are diagnosed", () =>
  graphs((a, b) => {
    const lock = json(join(b, "tapid.lock"));
    lock.npmLock.packages["node_modules/dep"].inBundle = true;
    writeJson(join(b, "tapid.lock"), lock);
    const result = compare(a, b);
    assert.equal(result.code, 1);
    assert.equal(result.report.instanceEvidence.tapid!.unsupported.length, 1);
  }));
test("equivalent differently hoisted installed trees preserve imported selections", () =>
  temporary((path) => {
    const [a, b] = ["npm", "tapid"].map((n) => join(path, n));
    const manifest = {
      name: "app",
      version: "1.0.0",
      dependencies: { other: "1.0.0" },
    };
    const packages: any = {
      "node_modules/other": {
        version: "1.0.0",
        resolved: "https://registry.npmjs.org/other/-/other-1.0.0.tgz",
        integrity: "sha512-other",
        dependencies: { dep: "1.0.0" },
      },
      "node_modules/other/node_modules/dep": {
        version: "1.0.0",
        resolved: "https://registry.npmjs.org/dep/-/dep-1.0.0.tgz",
        integrity: "sha512-dep",
      },
    };
    for (const root of [a, b]) {
      writeJson(join(root, "package.json"), manifest);
      writeJson(join(root, "node_modules/other/package.json"), {
        name: "other",
        version: "1.0.0",
        dependencies: { dep: "1.0.0" },
      });
      writeJson(
        join(
          root,
          root === a
            ? "node_modules/other/node_modules/dep/package.json"
            : "node_modules/dep/package.json",
        ),
        { name: "dep", version: "1.0.0" },
      );
      writeJson(join(root, "package-lock.json"), {
        lockfileVersion: 3,
        packages,
      });
      writeJson(join(root, "tapid.lock"), {
        lockfileVersion: 8,
        npmLock: { lockfileVersion: 3, packages },
        verifiedArtifacts: Object.fromEntries(
          Object.entries(packages).map(([id, r]: [string, any]) => [
            id,
            {
              treeDigest: "sha256-test",
              resolved: r.resolved,
              integrity: r.integrity,
            },
          ]),
        ),
      });
    }
    assert.equal(compare(a, b).code, 0);
  }));
test("peer provider mismatch fails even when dependency version is unchanged", () =>
  graphs((a, b) => {
    for (const root of [a, b]) {
      const version = root === a ? "2.0.0" : "3.0.0";
      const manifest = json(join(root, "package.json"));
      manifest.devDependencies = { peer: version };
      writeJson(join(root, "package.json"), manifest);
      writeJson(join(root, "node_modules/dep/package.json"), {
        name: "dep",
        version: "1.0.0",
        peerDependencies: { peer: "*" },
      });
      writeJson(join(root, "node_modules/peer/package.json"), {
        name: "peer",
        version,
      });
      const file = join(root, root === a ? "package-lock.json" : "tapid.lock"),
        lock = json(file),
        npm = root === a ? lock : lock.npmLock;
      npm.packages["node_modules/dep"].peerDependencies = { peer: "*" };
      npm.packages["node_modules/peer"] = {
        version,
        resolved: `https://registry.npmjs.org/peer/-/peer-${version}.tgz`,
        integrity: "sha512-peer",
      };
      if (root === b)
        lock.verifiedArtifacts["node_modules/peer"] = {
          ...npm.packages["node_modules/peer"],
          treeDigest: "sha256-test",
        };
      writeJson(file, lock);
    }
    const result = compare(a, b);
    assert.equal(result.code, 1);
    assert.ok(
      result.report.instanceEvidence.differences.some(
        (r) => r.field === "nodes",
      ),
    );
  }));
test("npm may retain an unreachable foreign optional branch descendant", () =>
  graphs((a, b) => {
    for (const root of [a, b]) {
      const file = join(root, root === a ? "package-lock.json" : "tapid.lock");
      const lock = json(file),
        npm = root === a ? lock : lock.npmLock;
      const manifest = json(join(root, "package.json"));
      manifest.optionalDependencies = { foreign: "1.0.0" };
      writeJson(join(root, "package.json"), manifest);
      npm.packages["node_modules/foreign"] = {
        version: "1.0.0",
        optional: true,
        os: ["nonexistent"],
        resolved: "https://registry.npmjs.org/foreign/-/foreign.tgz",
        integrity: "sha512-foreign",
        dependencies: { child: "1.0.0" },
      };
      npm.packages["node_modules/child"] = {
        version: "1.0.0",
        optional: true,
        resolved: "https://registry.npmjs.org/child/-/child.tgz",
        integrity: "sha512-child",
      };
      writeJson(file, lock);
    }
    writeJson(join(a, "node_modules/child/package.json"), {
      name: "child",
      version: "1.0.0",
    });
    const result = compare(a, b);
    assert.equal(result.code, 0);
    assert.deepEqual(
      result.report.permittedPhysicalDifferences.npmOnly.map((r) => r.name),
      ["child"],
    );
    // Removing the optional evidence must turn the same extra package into a failure.
    const lock = json(join(a, "package-lock.json"));
    delete lock.packages["node_modules/child"].optional;
    writeJson(join(a, "package-lock.json"), lock);
    assert.equal(compare(a, b).code, 1);
    lock.packages["node_modules/child"].optional = true;
    delete lock.packages["node_modules/foreign"].dependencies;
    writeJson(join(a, "package-lock.json"), lock);
    assert.equal(
      compare(a, b).code,
      1,
      "an optional orphan has no platform omission evidence",
    );
  }));
