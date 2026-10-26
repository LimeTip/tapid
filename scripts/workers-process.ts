// Unix process-group ownership for the Workers consumer lane.
import { spawn } from "node:child_process";
import { get } from "node:http";
import { writeFileSync } from "node:fs";
import { once } from "node:events";
export function isolatedEnvironment(
  home: string,
  inherited: NodeJS.ProcessEnv = process.env,
): NodeJS.ProcessEnv {
  return {
    PATH: inherited.PATH,
    SystemRoot: inherited.SystemRoot,
    HOME: home,
    USERPROFILE: home,
    XDG_CONFIG_HOME: home,
    XDG_CACHE_HOME: home,
    TMPDIR: home,
    TMP: home,
    TEMP: home,
    CI: "1",
    WRANGLER_SEND_METRICS: "false",
    WRANGLER_LOG_SANITIZE: "true",
    npm_config_cache: home + "/npm-cache",
    npm_config_userconfig: home + "/empty-npmrc",
    npm_config_registry: "https://registry.npmjs.org",
    NO_UPDATE_NOTIFIER: "1",
  };
}
function request(url: string): Promise<{ status: number; body: string }> {
  return new Promise((resolve, reject) => {
    const req = get(url, (res) => {
      const chunks: Buffer[] = [];
      let size = 0;
      res.on("data", (chunk: Buffer) => {
        size += chunk.length;
        if (size > 4096) req.destroy(new Error("response exceeds limit"));
        else chunks.push(chunk);
      });
      res.on("error", reject);
      res.on("end", () =>
        resolve({
          status: res.statusCode ?? 0,
          body: Buffer.concat(chunks).toString(),
        }),
      );
    });
    req.setTimeout(500, () => req.destroy(new Error("request timeout")));
    req.on("error", reject);
  });
}
export async function assertServer(
  args: string[],
  options: {
    cwd: string;
    env: NodeJS.ProcessEnv;
    url: string;
    expected: string;
    timeout?: number;
    log: string;
  },
): Promise<{ status: number; body: string }> {
  if (process.platform === "win32")
    throw new Error("Workers consumer process supervision requires Unix");
  const child = spawn(args[0], args.slice(1), {
    cwd: options.cwd,
    env: options.env,
    detached: true,
    stdio: ["ignore", "pipe", "pipe"],
  });
  let exited = false,
    failure: Error | null = null,
    bytes = 0;
  const chunks: Buffer[] = [];
  const closed = once(child, "close").catch(() => {});
  let terminationRequested = false;
  const kill = () => {
    if (terminationRequested || !child.pid) return;
    terminationRequested = true;
    if (child.pid)
      try {
        process.kill(-child.pid, "SIGKILL");
      } catch (error) {
        if ((error as NodeJS.ErrnoException).code !== "ESRCH") throw error;
      }
  };
  child.on("error", (error) => {
    failure = error;
  });
  child.on("exit", () => {
    exited = true;
  });
  for (const stream of [child.stdout, child.stderr])
    stream.on("data", (chunk: Buffer) => {
      const remaining = Math.max(0, 1048576 - bytes);
      chunks.push(chunk.subarray(0, remaining));
      bytes += chunk.length;
      if (bytes > 1048576 && !failure) {
        failure = new Error("server output limit");
        kill();
      }
    });
  const deadline = Date.now() + (options.timeout ?? 60000);
  try {
    while (Date.now() < deadline) {
      if (failure) throw failure;
      if (exited) throw new Error("server exited before readiness");
      let result: { status: number; body: string } | null = null;
      try {
        result = await request(options.url);
      } catch {
        /* readiness retries are bounded */
      }
      if (result) {
        if (result.status !== 200 || result.body !== options.expected)
          throw new Error(
            `incorrect response: ${result.status} ${JSON.stringify(result.body)}`,
          );
        if (exited || failure)
          throw failure ?? new Error("server exited during assertion");
        return result;
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
    throw new Error("server startup timeout");
  } finally {
    kill();
    child.stdout.destroy();
    child.stderr.destroy();
    await closed;
    writeFileSync(options.log, Buffer.concat(chunks));
  }
}
