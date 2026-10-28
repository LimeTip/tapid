import { strict as assert } from "node:assert";
import { test } from "node:test";
import { join } from "node:path";
import { root, json } from "./script-support.ts";
test("Workers fixture pins root tools and npm v3 baseline", () => {
  const dir = join(root, "examples/workers-consumer");
  const manifest = json(join(dir, "package.json"));
  const lock = json(join(dir, "package-lock.json"));
  assert.equal(manifest.engines.node, "22.23.3");
  assert.equal(manifest.packageManager, "npm@11.12.1");
  assert.equal(manifest.devDependencies.wrangler, "4.153.0");
  assert.equal(lock.lockfileVersion, 3);
  for (const field of ["dependencies", "devDependencies"]) {
    assert.deepEqual(lock.packages[""][field], manifest[field]);
    for (const [name, version] of Object.entries(manifest[field])) {
      assert.match(String(version), /^\d+\.\d+\.\d+/);
      assert.equal(lock.packages["node_modules/" + name].version, version);
    }
  }
});
test("every native transitive selection is pinned without conflicting overrides", () => {
  const dir = join(root, "examples/workers-consumer");
  const manifest = json(join(dir, "package.json")),
    lock = json(join(dir, "package-lock.json"));
  const selected = new Map<string, string>();
  for (const [path, record] of Object.entries(lock.packages) as [
    string,
    any,
  ][]) {
    if (!path) continue;
    const name = record.name ?? path.split("node_modules/").at(-1);
    assert.ok(
      !selected.has(name) || selected.get(name) === record.version,
      "conflicting global override " + name,
    );
    selected.set(name, record.version);
  }
  for (const [path, record] of Object.entries(lock.packages) as [
    string,
    any,
  ][]) {
    if (!path) continue;
    for (const field of ["dependencies", "optionalDependencies"])
      for (const [name, range] of Object.entries(record[field] ?? {})) {
        assert.equal(
          manifest.overrides?.[name] ??
            manifest.devDependencies?.[name] ??
            manifest.dependencies?.[name] ??
            range,
          selected.get(name),
          path + " -> " + name,
        );
      }
  }
});
test("fixture has only local Worker configuration with no cloud resources", () => {
  const config = json(join(root, "examples/workers-consumer/wrangler.jsonc"));
  assert.deepEqual(Object.keys(config).sort(), [
    "$schema",
    "compatibility_date",
    "main",
    "name",
    "send_metrics",
  ]);
  assert.equal(config.compatibility_date, "2026-10-23");
  assert.equal(config.send_metrics, false);
});
test("npm baseline retains glibc and musl optional package constraints", () => {
  const lock = json(join(root, "examples/workers-consumer/package-lock.json"));
  assert.deepEqual(lock.packages["node_modules/@img/sharp-linux-x64"].libc, [
    "glibc",
  ]);
  assert.deepEqual(
    lock.packages["node_modules/@img/sharp-linuxmusl-x64"].libc,
    ["musl"],
  );
});
