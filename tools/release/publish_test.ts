import { deepStrictEqual as assertEquals, rejects as assertRejects, strictEqual, throws as assertThrows } from "node:assert/strict";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { findCargoLockfiles, isPublished, parseCliArgs, planPublication, publicationPlan, publishPackages, registryState, renderHumanPlan, renderMachinePlan, waitForRegistryPublication, cargoPublishEnv, cargoMetadataEnv, type CargoMetadata } from "./publish.ts";

const metadata = {
  packages: [
    { name: "tapid", version: "0.0.7", dependencies: ["tapid-store", "tapid-linker", "tapid-lockfile", "tapid-resolver"] },
    { name: "tapid-store", version: "0.0.4", dependencies: ["tapid-archive", "tapid-core"] },
    { name: "tapid-archive", version: "0.0.3", dependencies: [] },
    { name: "tapid-core", version: "0.0.4", dependencies: [] },
    { name: "tapid-linker", version: "0.0.4", dependencies: ["tapid-core", "tapid-manifest"] },
    { name: "tapid-lockfile", version: "0.0.8", dependencies: ["tapid-core"] },
    { name: "tapid-manifest", version: "0.0.6", dependencies: ["tapid-core"] },
    { name: "tapid-registry-client", version: "0.0.4", dependencies: ["tapid-core"] },
    { name: "tapid-resolver", version: "0.0.4", dependencies: ["tapid-core", "tapid-registry-client"] },
    { name: "tapid-policy", version: "0.0.2", dependencies: [] },
  ],
};

test("discovers root and nested Cargo lockfiles without build or Git internals", async () => {
  const root = await mkdtemp(join(process.env.RUNNER_TEMP ?? tmpdir(), "tapid-lockfiles-"));
  try {
    await mkdir(join(root, "tests", "integration"), { recursive: true });
    await mkdir(join(root, "target", "fixture"), { recursive: true });
    await mkdir(join(root, ".git", "objects"), { recursive: true });
    for (const relative of [
      "Cargo.toml", "Cargo.lock",
      "tests/integration/Cargo.toml", "tests/integration/Cargo.lock",
      "target/fixture/Cargo.toml", "target/fixture/Cargo.lock",
      ".git/objects/Cargo.toml", ".git/objects/Cargo.lock",
    ]) await writeFile(join(root, relative), "");
    assertEquals(await findCargoLockfiles(root), ["Cargo.lock", "tests/integration/Cargo.lock"]);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("publication plan follows dependency order and skips published versions", () => {
  const published = new Set(["tapid-core@0.0.4", "tapid-manifest@0.0.6"]);
  assertEquals(publicationPlan(metadata, published), [
    { name: "tapid-archive", version: "0.0.3" },
    { name: "tapid-linker", version: "0.0.4" },
    { name: "tapid-lockfile", version: "0.0.8" },
    { name: "tapid-policy", version: "0.0.2" },
    { name: "tapid-registry-client", version: "0.0.4" },
    { name: "tapid-resolver", version: "0.0.4" },
    { name: "tapid-store", version: "0.0.4" },
    { name: "tapid", version: "0.0.7" },
  ]);
});

test("publication plan includes publishable workspace crates outside the tapid dependency closure", () => {
  const plan = publicationPlan(metadata, new Set());
  assertEquals(plan.some((pkg) => pkg.name === "tapid-policy"), true);
});

test("publication plan rejects a missing required package", () => {
  const incomplete = { packages: metadata.packages.filter((pkg) => pkg.name !== "tapid-store") };
  let error: unknown;
  try {
    publicationPlan(incomplete, new Set());
  } catch (caught) {
    error = caught;
  }
  assertEquals((error as Error).message, "cargo metadata is missing publishable package tapid-store");
});

test("publication plan rejects internal dependency cycles", () => {
  const cyclic = {
    packages: metadata.packages.map((pkg) => pkg.name === "tapid-store"
      ? { ...pkg, dependencies: [...pkg.dependencies, "tapid"] }
      : pkg),
  };
  let error: unknown;
  try {
    publicationPlan(cyclic, new Set());
  } catch (caught) {
    error = caught;
  }
  assertEquals((error as Error).message, "workspace dependency cycle includes tapid-store");
});

test("publication plan rejects an unpublishable workspace runtime dependency", () => {
  const unpublishable = {
    packages: [
      { name: "tapid", version: "1.0.0", dependencies: ["tapid-private"] },
      { name: "tapid-private", version: "1.0.0", dependencies: [], publish: [] },
    ],
  };
  assertRejects(
    async () => publicationPlan(unpublishable, new Set()),
    /workspace dependency tapid-private is not publishable to crates.io/,
  );
});

test("publication plan accepts a published version of a locally unpublishable dependency", () => {
  const publishedDependency = {
    packages: [
      { name: "tapid", version: "1.0.0", dependencies: ["itoa"] },
      { name: "itoa", version: "1.0.15", dependencies: [], publish: [] },
    ],
  };
  assertEquals(publicationPlan(publishedDependency, new Set(["itoa@1.0.15"])), [
    { name: "tapid", version: "1.0.0" },
  ]);
});

test("publication plan excludes packages restricted to another registry", () => {
  const restricted = {
    packages: [
      { name: "tapid", version: "1.0.0", dependencies: [] },
      { name: "tapid-private", version: "1.0.0", dependencies: [], publish: ["private"] },
    ],
  };
  assertEquals(publicationPlan(restricted, new Set()), [
    { name: "tapid", version: "1.0.0" },
  ]);
});

test("publication plan includes packages explicitly permitted for crates.io", () => {
  const explicit = {
    packages: [
      { name: "tapid", version: "1.0.0", dependencies: [] },
      { name: "tapid-public", version: "1.0.0", dependencies: [], publish: ["crates-io"] },
    ],
  };
  assertEquals(publicationPlan(explicit, new Set()), [
    { name: "tapid-public", version: "1.0.0" },
    { name: "tapid", version: "1.0.0" },
  ]);
});

test("publication plan includes local build dependencies and excludes dev and registry dependencies", () => {
  const objectMetadata = {
    packages: [
      {
        name: "tapid",
        version: "1.0.0",
        dependencies: [
          { name: "tapid-build", source: null, kind: "build" },
          { name: "tapid-dev", source: null, kind: "dev" },
          { name: "serde", source: "registry+https://github.com/rust-lang/crates.io-index", kind: null },
        ],
      },
      { name: "tapid-build", version: "1.0.0", dependencies: [] },
      { name: "tapid-dev", version: "1.0.0", dependencies: [], publish: [] },
    ],
  };
  assertEquals(publicationPlan(objectMetadata, new Set()), [
    { name: "tapid-build", version: "1.0.0" },
    { name: "tapid", version: "1.0.0" },
  ]);
});

test("crates.io lookup retries transient responses with bounded requests", async () => {
  const statuses = [429, 503, 200];
  let attempts = 0;
  const fakeFetch = async (_url: string | URL | Request, options?: RequestInit) => {
    attempts++;
    strictEqual(options?.signal instanceof AbortSignal, true);
    return new Response(null, { status: statuses.shift() });
  };
  strictEqual(await isPublished({ name: "tapid", version: "1.2.3" }, fakeFetch as typeof fetch, async () => {}), true);
  strictEqual(attempts, 3);
  await assertRejects(
    () => isPublished({ name: "tapid", version: "1.2.3" }, async () => new Response(null, { status: 403 }), async () => {}),
    /HTTP 403/,
  );
  await assertRejects(
    () => isPublished({ name: "tapid", version: "1.2.3" }, async () => new Response(null, { status: 204 }), async () => {}),
    /HTTP 204/,
  );
});

test("crates.io lookup honors Retry-After on rate limits", async () => {
  const delays: number[] = [];
  let attempts = 0;
  const published = await isPublished({ name: "tapid", version: "1.2.3" }, async () => {
    attempts++;
    return attempts === 1
      ? new Response(null, { status: 429, headers: { "Retry-After": "7" } })
      : new Response(null, { status: 200 });
  }, async (milliseconds) => { delays.push(milliseconds); });
  strictEqual(published, true);
  strictEqual(attempts, 2);
  assertEquals(delays, [7_000]);
});

test("crates.io lookup fails closed when Retry-After exceeds the bounded wait", async () => {
  let attempts = 0;
  await assertRejects(() => isPublished({ name: "tapid", version: "1.2.3" }, async () => {
    attempts++;
    return new Response(null, { status: 429, headers: { "Retry-After": "301" } });
  }, async () => {}), /retry delay over 5 minutes/);
  strictEqual(attempts, 1);
});

test("registry state distinguishes an exact missing version from public version drift", async () => {
  const state = await registryState({ name: "tapid", version: "2.0.0" }, async () => new Response(JSON.stringify({
    crate: { max_version: "3.0.0" },
    versions: [{ num: "3.0.0" }, { num: "1.0.0" }],
  }), { status: 200 }));
  assertEquals(state, { published: false, latestVersion: "3.0.0" });
});

test("registry state retries HTTP 429 and server errors with bounded backoff", async () => {
  const statuses = [429, 503, 200];
  const delays: number[] = [];
  let attempts = 0;
  const state = await registryState({ name: "tapid", version: "1.0.0" }, async () => {
    attempts++;
    return new Response(JSON.stringify({
      crate: { max_version: "1.0.0" },
      versions: [{ num: "1.0.0" }],
    }), { status: statuses.shift() });
  }, async (milliseconds: number) => { delays.push(milliseconds); });
  assertEquals(state, { published: true, latestVersion: "1.0.0" });
  assertEquals(attempts, 3);
  assertEquals(delays, [1_000, 2_000]);
});

test("registry state rejects an incomplete success response instead of treating it as unpublished", async () => {
  await assertRejects(() => registryState({ name: "tapid", version: "1.0.0" }, async () => new Response(
    JSON.stringify({ versions: [] }), { status: 200 },
  )), /malformed crates.io response for tapid/);
  await assertRejects(() => registryState({ name: "tapid", version: "1.0.0" }, async () => new Response(
    JSON.stringify({ crate: { max_version: "1.0.1" }, versions: [{ num: "1.0.0" }] }), { status: 200 },
  )), /malformed crates.io response for tapid/);
});

test("publication planner reports a deterministic no-op with verification commands", async () => {
  const result = await planPublication({
    ...metadata,
    lockfiles: ["Cargo.lock", "tests/integration/Cargo.lock"],
  }, async () => true);
  assertEquals(result.packages, []);
  assertEquals(result.blockers, []);
  assertEquals(result.lockfiles, ["Cargo.lock", "tests/integration/Cargo.lock"]);
  assertEquals(result.verification, [
    "cargo package --workspace --locked",
    "cargo metadata --manifest-path tests/integration/Cargo.toml --locked --format-version 1",
    'CARGO_HOME="$RUNNER_TEMP/clean-cargo-home" cargo install tapid --version 0.0.7 --locked --root "$RUNNER_TEMP/tapid-clean-install"',
    '"$RUNNER_TEMP/tapid-clean-install/bin/tapid" --version',
  ]);
  strictEqual(renderMachinePlan(result), JSON.stringify(result, null, 2) + "\n");
  strictEqual(renderHumanPlan(result), "No crates.io packages require publication.\n");
});

test("publication planner includes changed dependents and records recovery guidance", async () => {
  const result = await planPublication(metadata, async (pkg) => pkg.name === "tapid-core" && pkg.version === "0.0.4");
  assertEquals(result.packages.map((pkg) => pkg.name), [
    "tapid-archive", "tapid-manifest", "tapid-linker", "tapid-lockfile", "tapid-policy",
    "tapid-registry-client", "tapid-resolver", "tapid-store", "tapid",
  ]);
  assertEquals(result.recovery, "Record confirmed package versions, query crates.io again, and rerun this read-only plan; resume only with the remaining dependency-ordered suffix.");
});

test("publication planning preserves metadata and fails closed on registry errors", async () => {
  const snapshot = JSON.stringify(metadata);
  const result = await planPublication(metadata, async () => {
    throw new Error("HTTP 429 from crates.io");
  });
  assertEquals(result.packages, []);
  assertEquals(result.blockers.length, publicationPlan(metadata, new Set()).length);
  for (const blocker of result.blockers) strictEqual(blocker.endsWith(": HTTP 429 from crates.io"), true);
  strictEqual(JSON.stringify(metadata), snapshot);
});

test("publication planner checks registry before rejecting an unpublishable local stand-in", async () => {
  const standIn = {
    packages: [
      { name: "tapid", version: "1.0.0", dependencies: ["tapid-private"] },
      { name: "tapid-private", version: "1.0.0", dependencies: [], publish: [] },
    ],
  };
  const lookedUp: string[] = [];
  const result = await planPublication(standIn, async (pkg) => {
    lookedUp.push(`${pkg.name}@${pkg.version}`);
    return pkg.name === "tapid-private";
  });
  assertEquals(lookedUp, ["tapid@1.0.0", "tapid-private@1.0.0"]);
  assertEquals(result.blockers, []);
  assertEquals(result.packages, [{ name: "tapid", version: "1.0.0" }]);
});

test("publication planner reports missing and drifted registry versions and dependent bumps", async () => {
  const input: CargoMetadata = {
    packages: [
      { name: "tapid", version: "1.0.0", dependencies: [{ name: "tapid-core", source: null, kind: null }] },
      { name: "tapid-core", version: "2.0.0", dependencies: [] },
    ],
    lockfiles: ["Cargo.lock", "tests/integration/Cargo.lock"],
  };
  const plan = await planPublication(input, async (pkg) => pkg.name === "tapid-core"
    ? { published: false, latestVersion: "1.0.0" }
    : { published: false });
  assertEquals(plan.drift, [{ name: "tapid-core", localVersion: "2.0.0", publicVersion: "1.0.0" }]);
  assertEquals(plan.missing, [{ name: "tapid-core", version: "2.0.0" }, { name: "tapid", version: "1.0.0" }]);
  assertEquals(plan.dependentBumps, [{ dependent: "tapid", dependency: "tapid-core", requiredVersion: "2.0.0" }]);
  assertEquals(plan.lockfiles, ["Cargo.lock", "tests/integration/Cargo.lock"]);
  assertEquals(plan.verification, [
    "cargo package --workspace --locked",
    "cargo metadata --manifest-path tests/integration/Cargo.toml --locked --format-version 1",
    'CARGO_HOME="$RUNNER_TEMP/clean-cargo-home" cargo install tapid --version 1.0.0 --locked --root "$RUNNER_TEMP/tapid-clean-install"',
    '"$RUNNER_TEMP/tapid-clean-install/bin/tapid" --version',
  ]);
});

test("publication planning blocks an older local package version before mutation", async () => {
  const result = await planPublication({
    packages: [{ name: "tapid", version: "1.0.0", dependencies: [] }],
  }, async () => ({ published: false, latestVersion: "2.0.0" }));
  assertEquals(result.packages, []);
  assertEquals(result.blockers, ["tapid@1.0.0 is older than published crates.io version 2.0.0"]);
});

test("publication planner fails closed on package verification and path/registry incompatibility", async () => {
  const input: CargoMetadata = {
    packages: [
      { name: "tapid", version: "1.0.0", dependencies: [{ name: "tapid-core", source: "registry+https://example.com", path: "../tapid-core", kind: null }], packageVerificationError: "cargo package failed" },
      { name: "tapid-core", version: "1.0.0", dependencies: [] },
    ],
  };
  const plan = await planPublication(input, async () => false);
  assertEquals(plan.packages, []);
  assertEquals(plan.blockers, [
    "tapid: cargo package failed",
    "tapid: local path dependency tapid-core has registry source registry+https://example.com",
  ]);
});

test("publication planner emits byte-stable JSON", async () => {
  const first = await planPublication(metadata, async () => false);
  const second = await planPublication({ packages: [...metadata.packages].reverse() }, async () => false);
  strictEqual(renderMachinePlan(first), renderMachinePlan(second));
});

test("publishes crates sequentially and waits for registry read-back before the next dependent", async () => {
  const events: string[] = [];
  await publishPackages([
    { name: "tapid-core", version: "1.0.0" },
    { name: "tapid", version: "1.0.0" },
  ], {
    isPublished: async (pkg) => {
      events.push(`check:${pkg.name}`);
      return false;
    },
    publish: async (pkg) => { events.push(`publish:${pkg.name}`); },
    waitForPublished: async (pkg) => { events.push(`read-back:${pkg.name}`); },
  });
  assertEquals(events, [
    "check:tapid-core", "publish:tapid-core", "read-back:tapid-core",
    "check:tapid", "publish:tapid", "read-back:tapid",
  ]);
});

test("stops before later crates when publication fails", async () => {
  const events: string[] = [];
  await assertRejects(() => publishPackages([
    { name: "tapid-core", version: "1.0.0" },
    { name: "tapid", version: "1.0.0" },
  ], {
    isPublished: async (pkg) => { events.push(`check:${pkg.name}`); return false; },
    publish: async (pkg) => {
      events.push(`publish:${pkg.name}`);
      if (pkg.name === "tapid-core") throw new Error("cargo publish failed");
    },
    waitForPublished: async (pkg) => { events.push(`read-back:${pkg.name}`); },
  }), /cargo publish failed/);
  assertEquals(events, ["check:tapid-core", "publish:tapid-core"]);
});

test("Cargo metadata subprocess receives no registry credentials or unrelated secrets", () => {
  assertEquals(cargoMetadataEnv({
    PATH: "/bin",
    HOME: "/home/runner",
    CARGO_HOME: "/shared/cargo-home",
    RUSTUP_HOME: "/rustup",
    CARGO_REGISTRY_TOKEN: "registry-token",
    CARGO_REGISTRIES_PRIVATE_TOKEN: "private-token",
    GITHUB_TOKEN: "github-token",
    UNRELATED_SECRET: "secret-value",
  }, "/isolated/metadata-home"), {
    PATH: "/bin",
    HOME: "/home/runner",
    CARGO_HOME: "/isolated/metadata-home",
    RUSTUP_HOME: "/rustup",
  });
});

test("publish subprocess receives the OIDC token but no unrelated environment secrets", () => {
  assertEquals(cargoPublishEnv({
    PATH: "/bin",
    HOME: "/home/runner",
    CARGO_HOME: "/shared/cargo-home",
    RUSTUP_HOME: "/rustup",
    HTTP_PROXY: "http://proxy.example.com",
    CARGO_REGISTRY_TOKEN: "old-token",
    CARGO_REGISTRIES_PRIVATE_TOKEN: "private-token",
    GITHUB_TOKEN: "github-token",
    UNRELATED_SECRET: "secret-value",
  }, "oidc-token", "/isolated/publish-home"), {
    PATH: "/bin",
    HOME: "/home/runner",
    CARGO_HOME: "/isolated/publish-home",
    RUSTUP_HOME: "/rustup",
    HTTP_PROXY: "http://proxy.example.com",
    CARGO_REGISTRY_TOKEN: "oidc-token",
  });
});

test("publication execution is opt-in and accepts a workspace root", () => {
  assertEquals(parseCliArgs([], "/trusted/main"), {
    publish: false, json: false, workspaceDir: "/trusted/main", expectedPlanPath: undefined,
  });
  assertEquals(parseCliArgs(["--publish", "--json", "--workspace", "../release-source", "--expected-plan", "plan.json"], "/trusted/main"), {
    publish: true, json: true, workspaceDir: "/trusted/release-source", expectedPlanPath: "/trusted/main/plan.json",
  });
  assertThrows(() => parseCliArgs(["--publsh"]), /unknown option/);
});

test("publication read-back waits until the exact version is visible", async () => {
  const delays: number[] = [];
  let checks = 0;
  await waitForRegistryPublication(
    { name: "tapid-core", version: "1.0.0" },
    async () => ++checks === 3,
    async (milliseconds: number) => { delays.push(milliseconds); },
    5,
  );
  assertEquals(checks, 3);
  assertEquals(delays, [5_000, 10_000]);
});
