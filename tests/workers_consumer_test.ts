import { strict as assert } from "node:assert";
import { test } from "node:test";
import { join } from "node:path";
import { temporary, read } from "./script-support.ts";
import {
  assertServer,
  isolatedEnvironment,
} from "../scripts/workers-process.ts";
test("tool environment excludes inherited credentials and overrides", () => {
  const env = isolatedEnvironment("/isolated", {
    PATH: "/bin",
    CLOUDFLARE_API_TOKEN: "secret",
    CF_API_KEY: "secret",
    NODE_OPTIONS: "unsafe",
    WORKERD_BINARY_PATH: "/outside",
    HOME: "/real",
  });
  assert.equal(env.CLOUDFLARE_API_TOKEN, undefined);
  assert.equal(env.WORKERD_BINARY_PATH, undefined);
  assert.equal(env.NODE_OPTIONS, undefined);
  assert.equal(env.HOME, "/isolated");
});
const unix = {
  skip:
    process.platform === "win32"
      ? "Workers supervision uses Unix process groups"
      : false,
};
for (const [name, program, expected] of [
  ["early exit", "process.exit(3)", "exited"],
  ["startup timeout", "setInterval(()=>{},1000)", "timeout"],
] as const)
  test(name, unix, () =>
    temporary(async (path) => {
      const log = join(path, "server.log");
      await assert.rejects(
        assertServer([process.execPath, "-e", program], {
          cwd: path,
          env: isolatedEnvironment(path),
          url: "http://127.0.0.1:1",
          expected: "marker",
          timeout: 300,
          log,
        }),
        new RegExp(expected),
      );
      assert.equal(typeof read(log), "string");
    }),
  );
import { createServer } from "node:net";
async function freePort() {
  const server = createServer();
  return await new Promise<number>((resolve, reject) => {
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const port = (server.address() as any).port;
      server.close((error) => (error ? reject(error) : resolve(port)));
    });
  });
}
for (const [body, expected] of [
  ["marker", null],
  ["wrong", "incorrect response"],
] as const)
  test("response assertion and cleanup: " + body, unix, () =>
    temporary(async (path) => {
      const port = await freePort();
      const pidFile = join(path, "pid");
      const program = `require('node:fs').writeFileSync(${JSON.stringify(pidFile)},String(process.pid));require('node:http').createServer((q,r)=>r.end(${JSON.stringify(body)})).listen(${port},'127.0.0.1')`;
      const call = assertServer([process.execPath, "-e", program], {
        cwd: path,
        env: isolatedEnvironment(path),
        url: `http://127.0.0.1:${port}`,
        expected: "marker",
        timeout: 2000,
        log: join(path, "server.log"),
      });
      if (expected) await assert.rejects(call, new RegExp(expected));
      else assert.deepEqual(await call, { status: 200, body: "marker" });
      const pid = Number(read(pidFile));
      assert.throws(
        () => process.kill(pid, 0),
        (error: any) => error.code === "ESRCH",
      );
    }),
  );
test("spawn failure retains a log", unix, () =>
  temporary(async (path) => {
    const log = join(path, "failure.log");
    await assert.rejects(
      assertServer([join(path, "missing")], {
        cwd: path,
        env: isolatedEnvironment(path),
        url: "http://127.0.0.1:1",
        expected: "marker",
        timeout: 300,
        log,
      }),
      /ENOENT/,
    );
    assert.equal(read(log), "");
  }),
);
test("output overflow fails and cleans up", unix, () =>
  temporary(async (path) => {
    const log = join(path, "overflow.log");
    await assert.rejects(
      assertServer(
        [
          process.execPath,
          "-e",
          "setInterval(()=>process.stdout.write('x'.repeat(65536)),1)",
        ],
        {
          cwd: path,
          env: isolatedEnvironment(path),
          url: "http://127.0.0.1:1",
          expected: "marker",
          timeout: 2000,
          log,
        },
      ),
      /output limit/,
    );
    assert.ok(Buffer.byteLength(read(log)) <= 1048576);
  }),
);
test("server descendants are terminated on failed response", unix, () =>
  temporary(async (path) => {
    const port = await freePort(),
      file = join(path, "descendant");
    const program = `const child=require('node:child_process').spawn(process.execPath,['-e','setInterval(()=>{},1000)'],{stdio:'inherit'});require('node:fs').writeFileSync(${JSON.stringify(file)},String(child.pid));require('node:http').createServer((q,r)=>r.end('wrong')).listen(${port},'127.0.0.1')`;
    await assert.rejects(
      assertServer([process.execPath, "-e", program], {
        cwd: path,
        env: isolatedEnvironment(path),
        url: `http://127.0.0.1:${port}`,
        expected: "marker",
        timeout: 2000,
        log: join(path, "server.log"),
      }),
      /incorrect response/,
    );
    const pid = Number(read(file));
    let alive = true;
    for (let i = 0; i < 40 && alive; i++) {
      try {
        process.kill(pid, 0);
        await new Promise((resolve) => setTimeout(resolve, 25));
      } catch (error) {
        assert.equal((error as NodeJS.ErrnoException).code, "ESRCH");
        alive = false;
      }
    }
    assert.equal(alive, false, "descendant survived failed assertion");
  }),
);
