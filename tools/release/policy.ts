import { execFile } from "node:child_process";
import { resolve } from "node:path";
import { promisify } from "node:util";
import { pathToFileURL } from "node:url";

const guidance = "Configure the release protections described in docs/release-automation.md before retrying";
function fail(reason: string): never { throw new Error(`${reason}. ${guidance}.`); }
function object(value: unknown): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) fail("Malformed GitHub policy response");
  return value as Record<string, unknown>;
}
function array(value: unknown): unknown[] {
  if (!Array.isArray(value)) fail("Malformed GitHub policy list");
  return value;
}

function environmentPolicy(environment: unknown, policies: unknown, name: string, approved: boolean): void {
  const env = object(environment), branch = object(env.deployment_branch_policy), policy = object(policies);
  if (env.name !== name || env.can_admins_bypass !== false) fail(`${name} must disable administrator environment bypass`);
  const entries = array(policy.branch_policies);
  if (branch.custom_branch_policies !== true || branch.protected_branches !== false || policy.total_count !== 1 || entries.length !== 1 || object(entries[0]).name !== "main" || object(entries[0]).type !== "branch") {
    fail(`${name} must allow only the main branch, without tag policies`);
  }
  const rules = array(env.protection_rules).map(object).filter((rule) => rule.type === "required_reviewers");
  if (approved) {
    if (rules.length !== 1 || rules[0].prevent_self_review !== true || array(rules[0].reviewers).length === 0) fail(`${name} requires an independent reviewer and must prevent self-approval`);
    for (const reviewer of array(rules[0].reviewers).map(object)) {
      const id = object(reviewer.reviewer).id;
      if (!["User", "Team"].includes(String(reviewer.type)) || !Number.isSafeInteger(id) || Number(id) < 1) fail(`${name} has a malformed required reviewer`);
    }
  } else if (rules.some((rule) => array(rule.reviewers).length !== 0)) {
    fail(`${name} must have no separate reviewers under the one-release-approval policy`);
  }
}

function matchesMain(pattern: unknown, defaultBranch: unknown): boolean {
  return pattern === "~ALL" || pattern === "refs/heads/main" || (pattern === "~DEFAULT_BRANCH" && defaultBranch === "main");
}

/** Read-only policy validation. Malformed or inaccessible protections never permit publication. */
export function validateReleasePolicy(value: unknown): void {
  const state = object(value);
  environmentPolicy(state.stableRelease, state.stablePolicies, "stable-release", true);
  environmentPolicy(state.cratesRelease, state.cratesPolicies, "crates-io-release", false);
  const mainRules = array(state.rulesets).map(object).filter((ruleset) => {
    if (ruleset.target !== "branch" || ruleset.enforcement !== "active") return false;
    const refs = object(object(ruleset.conditions).ref_name);
    return array(refs.include).some((pattern) => matchesMain(pattern, state.defaultBranch)) && array(refs.exclude).length === 0;
  }).flatMap((ruleset) => array(ruleset.rules).map(object));
  const freshness = mainRules.some((rule) => {
    if (rule.type !== "required_status_checks") return false;
    const parameters = object(rule.parameters);
    return parameters.strict_required_status_checks_policy === true && array(parameters.required_status_checks).map(object).some((check) => check.context === "Release intent freshness" && check.integration_id === 15368);
  });
  if (!freshness) fail("An active main ruleset must strictly require Release intent freshness from GitHub Actions integration 15368");
  const review = mainRules.some((rule) => {
    if (rule.type !== "pull_request") return false;
    const parameters = object(rule.parameters), count = parameters.required_approving_review_count;
    return Number.isSafeInteger(count) && Number(count) >= 1 && parameters.dismiss_stale_reviews_on_push === true && parameters.require_last_push_approval === true;
  });
  if (!review) fail("An active main ruleset must require independent PR review, stale review dismissal, and last-push approval");
}

const execFileAsync = promisify(execFile);
async function check(): Promise<void> {
  const repository = process.env.GITHUB_REPOSITORY;
  if (!repository || !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/.test(repository)) fail("GITHUB_REPOSITORY must identify the release repository");
  const api = async (endpoint: string) => {
    try {
      const path = `repos/${repository}${endpoint ? `/${endpoint}` : ""}`;
      const { stdout } = await execFileAsync("gh", ["api", path], { encoding: "utf8", maxBuffer: 8 * 1024 * 1024 });
      return JSON.parse(stdout) as unknown;
    } catch { return fail(`Cannot read GitHub release policy at ${endpoint || "repository"}; verify the workflow token can read environments and rulesets`); }
  };
  const [repo, stableRelease, stablePolicies, cratesRelease, cratesPolicies, summaries] = await Promise.all([
    api(""), api("environments/stable-release"), api("environments/stable-release/deployment-branch-policies?per_page=100"), api("environments/crates-io-release"), api("environments/crates-io-release/deployment-branch-policies?per_page=100"), api("rulesets?includes_parents=true&per_page=100"),
  ]);
  const listed = array(summaries).map(object);
  if (listed.length === 100) fail("Ruleset listing may be truncated; cannot safely confirm main protections");
  const rulesets = await Promise.all(listed.filter((rule) => rule.target === "branch" && rule.enforcement === "active").map((rule) => {
    if (!Number.isSafeInteger(rule.id) || Number(rule.id) < 1) fail("Malformed GitHub ruleset identifier");
    return api(`rulesets/${rule.id}`);
  }));
  validateReleasePolicy({ defaultBranch: object(repo).default_branch, stableRelease, stablePolicies, cratesRelease, cratesPolicies, rulesets });
  console.log("GitHub release protection policy verified");
}

if (process.argv[1] && pathToFileURL(resolve(process.argv[1])).href === import.meta.url) {
  if (process.argv.slice(2).join(" ") !== "check") fail("usage: policy.ts check");
  await check();
}
