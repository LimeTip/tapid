import { execFile } from "node:child_process";
import { readFile } from "node:fs/promises";
import { resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";
import { validateReleaseIntent } from "./candidate.ts";

const exec = promisify(execFile);
const maxWaitMs = 90 * 60 * 1000;
type CiRun = { id: number; run_number: number; head_sha: string; head_branch: string; event: string; status: string; conclusion: string | null };

function commit(sha: string): void {
  if (!/^[a-f0-9]{40}$/.test(sha)) throw Error("invalid release source commit");
}

export function evaluateCi(input: unknown, sha: string): "waiting" | "success" {
  commit(sha);
  if (!Array.isArray(input)) throw Error("invalid CI run collection");
  const matching = input.filter((run): run is CiRun => !!run && typeof run === "object" && run.head_sha === sha && run.head_branch === "main" && run.event === "push");
  for (const run of matching) {
    if (!Number.isSafeInteger(run.id) || run.id <= 0 || !Number.isSafeInteger(run.run_number) || run.run_number <= 0) throw Error("invalid CI run identity");
  }
  const latest = matching.sort((a, b) => b.run_number - a.run_number || b.id - a.id)[0];
  if (!latest) return "waiting";
  if (["queued", "in_progress", "waiting", "pending", "requested"].includes(latest.status)) return "waiting";
  if (latest.status !== "completed") throw Error("invalid CI run status");
  if (latest.conclusion !== "success") throw Error(`main CI did not succeed for ${sha}: ${latest.conclusion}`);
  return "success";
}

// GitHub's default CodeQL setup lives outside repository workflow files.
export const requiredChecks = ['Analyze (actions)', 'Analyze (rust)', 'Analyze (javascript-typescript)', 'Analyze (python)', 'Command help coverage'];
export function evaluateChecks(input: unknown, sha: string): 'waiting' | 'success' {
  commit(sha);
  if (!Array.isArray(input)) throw Error('invalid check run collection');
  let waiting = false;
  for (const name of requiredChecks) {
    const checks = input.filter(check => check && check.name === name && check.head_sha === sha && check.app?.id === 15368);
    for (const check of checks) if (!Number.isSafeInteger(check.id) || check.id <= 0) throw Error('invalid check run identity');
    const latest = checks.sort((a, b) => b.id - a.id)[0];
    if (!latest || ['queued', 'in_progress', 'pending', 'waiting', 'requested'].includes(latest.status)) { waiting = true; continue; }
    if (latest.status !== 'completed' || latest.conclusion !== 'success') throw Error(`required check did not succeed for ${sha}: ${name}`);
  }
  return waiting ? 'waiting' : 'success';
}

export async function waitForCi(options: {
  sha: string;
  readRuns: () => Promise<unknown>;
  readChecks?: () => Promise<unknown>;
  now?: () => number;
  sleep?: (ms: number) => Promise<void>;
  timeoutMs?: number;
  pollIntervalMs?: number;
}): Promise<void> {
  commit(options.sha);
  const now = options.now ?? Date.now;
  const sleep = options.sleep ?? ((ms) => new Promise<void>((done) => setTimeout(done, ms)));
  const timeout = options.timeoutMs ?? maxWaitMs;
  const interval = options.pollIntervalMs ?? 30000;
  if (!Number.isSafeInteger(timeout) || timeout <= 0 || timeout > maxWaitMs || !Number.isSafeInteger(interval) || interval <= 0) throw Error("invalid CI wait bounds");
  const deadline = now() + timeout;
  while (true) {
    const ci = evaluateCi(await options.readRuns(), options.sha);
    const checks = options.readChecks ? evaluateChecks(await options.readChecks(), options.sha) : "success";
    if (ci === "success" && checks === "success") return;
    const remaining = deadline - now();
    if (remaining <= 0) throw Error(`timed out waiting for exact main CI: ${options.sha}`);
    await sleep(Math.min(interval, remaining));
  }
}

function record(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) throw Error("invalid publication evidence");
  return value as Record<string, unknown>;
}

export function checkApprovedPlan(intentInput: unknown, planInput: unknown, metadataInput: unknown): void {
  const intent = validateReleaseIntent(intentInput);
  const plan = record(planInput), metadata = record(metadataInput);
  if (!Array.isArray(plan.blockers) || plan.blockers.length !== 0) throw Error("publication plan has blockers or missing blocker evidence");
  if (!Array.isArray(plan.packages) || plan.packages.length > intent.packages.length) throw Error("publication plan is not an approved suffix");
  const remaining = intent.packages.slice(intent.packages.length - plan.packages.length);
  for (let i = 0; i < remaining.length; i++) {
    const actual = record(plan.packages[i]);
    if (Object.keys(actual).sort().join(",") !== "name,version" || actual.name !== remaining[i].name || actual.version !== remaining[i].version) throw Error("publication plan is not an approved suffix");
  }
  if (!Array.isArray(metadata.packages)) throw Error("missing source metadata");
  for (const approved of intent.packages) {
    const matches = metadata.packages.filter((pkg) => pkg && typeof pkg === "object" && pkg.name === approved.name);
    if (matches.length !== 1 || matches[0].version !== approved.version) throw Error(`approved package differs from source metadata: ${approved.name}`);
  }
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  const [command, ...args] = process.argv.slice(2);
  if (command === "wait-ci" && args.length === 1) {
    const repository = process.env.GITHUB_REPOSITORY;
    if (!repository || !/^[A-Za-z0-9][A-Za-z0-9_-]*\/[A-Za-z0-9][A-Za-z0-9._-]*$/.test(repository)) throw Error("invalid GitHub repository");
    commit(args[0]);
    await waitForCi({ sha: args[0], readRuns: async () => {
      const { stdout } = await exec("gh", ["api", `repos/${repository}/actions/workflows/ci.yml/runs?event=push&head_sha=${args[0]}&per_page=100`], { maxBuffer: 8 * 1024 * 1024, timeout: 60000 });
      return JSON.parse(stdout).workflow_runs;
    }, readChecks: async () => {
      const { stdout } = await exec("gh", ["api", `repos/${repository}/commits/${args[0]}/check-runs?per_page=100`, "--paginate", "--slurp"], { maxBuffer: 8 * 1024 * 1024, timeout: 60000 });
      return JSON.parse(stdout).flatMap((page: {check_runs: unknown[]}) => page.check_runs);
    } });
    console.log(`Exact main CI passed: ${args[0]}`);
  } else if (command === "check-plan" && args.length === 3) {
    const evidence = await Promise.all(args.map(async (path) => JSON.parse(await readFile(path, "utf8"))));
    checkApprovedPlan(...evidence as [unknown, unknown, unknown]);
    console.log("Publication plan matches the approved candidate and exact source metadata");
  } else throw Error("usage: automation.ts wait-ci SHA | check-plan INTENT PLAN METADATA");
}
