import { deepStrictEqual as assertEquals, rejects as assertRejects, strictEqual } from "node:assert/strict";
import { test } from "node:test";
import { isPublished, publicationPlan } from "./publish.ts";

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

test("publication plan follows dependency order and skips published versions", () => {
  const published = new Set(["tapid-core@0.0.4", "tapid-manifest@0.0.6"]);
  assertEquals(publicationPlan(metadata, published), [
    { name: "tapid-archive", version: "0.0.3" },
    { name: "tapid-store", version: "0.0.4" },
    { name: "tapid-linker", version: "0.0.4" },
    { name: "tapid-lockfile", version: "0.0.8" },
    { name: "tapid-registry-client", version: "0.0.4" },
    { name: "tapid-resolver", version: "0.0.4" },
    { name: "tapid", version: "0.0.7" },
  ]);
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
  assertEquals((error as Error).message, "workspace dependency cycle includes tapid");
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
});
