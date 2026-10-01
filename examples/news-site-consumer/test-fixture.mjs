import { readFile } from "node:fs/promises";
import { test } from "node:test";
import assert from "node:assert/strict";

const marker = "TAPID_NEWS_SITE_ACCEPTANCE_V1";

for (const [label, file] of [
  ["server-rendered news page", "app/page.tsx"],
  ["acceptance endpoint", "app/acceptance/route.ts"],
]) {
  test(`${label} includes the unique public marker`, async () => {
    const source = await readFile(new URL(file, import.meta.url), "utf8");
    assert.ok(source.includes(marker));
  });
}
