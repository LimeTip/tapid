import { readFile } from "node:fs/promises";
import assert from "node:assert/strict";
import test from "node:test";

const marker = "TAPID_NEWS_SITE_ACCEPTANCE_V1";

test("server-rendered news page contains representative synthetic headlines", async () => {
  const page = await readFile(new URL("./app/page.tsx", import.meta.url), "utf8");
  assert.match(page, /export default function Home/);
  assert.match(page, /Synthetic daily briefing/);
  assert.match(page, /Local library opens a new community newsroom/);
  assert.match(page, /Harbor cleanup brings volunteers together/);
});

test("live acceptance route declares the unique marker", async () => {
  const route = await readFile(new URL("./app/acceptance/route.ts", import.meta.url), "utf8");
  assert.match(route, /export function GET\(\)/);
  assert.match(route, new RegExp(marker));
});
