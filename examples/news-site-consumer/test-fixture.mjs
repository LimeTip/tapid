import assert from "node:assert/strict";
import test from "node:test";

import { app } from "./app.mjs";

const marker = "TAPID_NEWS_SITE_ACCEPTANCE_V1";

test("home page renders synthetic headlines as HTML", async () => {
  const response = await app.fetch(new Request("http://localhost/"));
  assert.equal(response.status, 200);
  assert.match(response.headers.get("content-type"), /text\/html/);
  assert.match(await response.text(), /Synthetic daily briefing/);
});

test("acceptance route returns the unique marker", async () => {
  const response = await app.fetch(new Request("http://localhost/acceptance"));
  assert.equal(response.status, 200);
  assert.equal(await response.text(), marker);
});
