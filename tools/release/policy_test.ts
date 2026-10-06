import { deepEqual, throws } from "node:assert/strict";
import { test } from "node:test";
import { validateReleasePolicy } from "./policy.ts";

function fixture() {
  const environment = (name: string, approved: boolean) => ({ name, can_admins_bypass: false, deployment_branch_policy: { protected_branches: false, custom_branch_policies: true }, protection_rules: approved ? [{ type: "required_reviewers", prevent_self_review: true, reviewers: [{ type: "User", reviewer: { id: 1 } }] }] : [] });
  const policies = () => ({ total_count: 1, branch_policies: [{ name: "main", type: "branch" }] });
  return { defaultBranch: "main", stableRelease: environment("stable-release", true), stablePolicies: policies(), cratesRelease: environment("crates-io-release", false), cratesPolicies: policies(), rulesets: [{ target: "branch", enforcement: "active", conditions: { ref_name: { include: ["~DEFAULT_BRANCH"], exclude: [] } }, rules: [
    { type: "required_status_checks", parameters: { strict_required_status_checks_policy: true, required_status_checks: [{ context: "Release intent freshness", integration_id: 15368 }] } },
    { type: "pull_request", parameters: { required_approving_review_count: 1, dismiss_stale_reviews_on_push: true, require_last_push_approval: true } },
  ] }] };
}

test("one independent approval and enforced freshness permit automation", () => {
  deepEqual(validateReleasePolicy(fixture()), { additionalCratesApproval: false });
});

test("activation permits the existing crates reviewer and reports the additional approval", () => {
  const state = fixture();
  state.cratesRelease.protection_rules = structuredClone(state.stableRelease.protection_rules);
  deepEqual(validateReleasePolicy(state), { additionalCratesApproval: true });
  state.stableRelease.protection_rules = [];
  throws(() => validateReleasePolicy(state), /independent reviewer/);
});

test("an extra crates approval cannot substitute for publication protections", () => {
  for (const mutate of [
    (state: ReturnType<typeof fixture>) => { state.cratesRelease.can_admins_bypass = true; },
    (state: ReturnType<typeof fixture>) => { state.cratesPolicies.branch_policies[0].name = "*"; },
    (state: ReturnType<typeof fixture>) => { state.cratesRelease.protection_rules[0].reviewers[0].reviewer.id = 0; },
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].enforcement = "evaluate"; },
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].rules[1].parameters.require_last_push_approval = false; },
  ]) {
    const state = fixture();
    state.cratesRelease.protection_rules = structuredClone(state.stableRelease.protection_rules);
    mutate(state);
    throws(() => validateReleasePolicy(state), /docs\/release-automation\.md/);
  }
});

test("missing or bypassable environment approval fails with setup guidance", () => {
  for (const mutate of [
    (state: ReturnType<typeof fixture>) => { state.stableRelease.can_admins_bypass = true; },
    (state: ReturnType<typeof fixture>) => { state.stableRelease.protection_rules[0].prevent_self_review = false; },
    (state: ReturnType<typeof fixture>) => { state.stableRelease.protection_rules[0].reviewers = []; },
    (state: ReturnType<typeof fixture>) => { state.stableRelease.protection_rules = []; },
    (state: ReturnType<typeof fixture>) => { state.cratesRelease.can_admins_bypass = true; },
  ]) {
    const state = fixture(); mutate(state);
    throws(() => validateReleasePolicy(state), /docs\/release-automation\.md/);
  }
});

test("environment branch policies require exactly main and reject tags", () => {
  for (const mutate of [
    (state: ReturnType<typeof fixture>) => { state.stablePolicies.branch_policies[0].name = "*"; },
    (state: ReturnType<typeof fixture>) => { state.cratesPolicies.branch_policies[0].type = "tag"; },
    (state: ReturnType<typeof fixture>) => { state.stablePolicies.total_count = 2; },
    (state: ReturnType<typeof fixture>) => { state.cratesRelease.deployment_branch_policy.protected_branches = true; },
  ]) { const state = fixture(); mutate(state); throws(() => validateReleasePolicy(state), /main/); }
});

test("freshness and independent PR review must be active, strict, and bind GitHub Actions", () => {
  for (const mutate of [
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].enforcement = "evaluate"; },
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].conditions.ref_name.exclude = ["refs/heads/main"]; },
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].conditions.ref_name.include = ["refs/heads/release/*"]; },
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].rules[0].parameters.strict_required_status_checks_policy = false; },
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].rules[0].parameters.required_status_checks![0].integration_id = 1; },
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].rules[0].parameters.required_status_checks![0].context = "Different check"; },
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].rules[1].parameters.required_approving_review_count = 0; },
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].rules[1].parameters.dismiss_stale_reviews_on_push = false; },
    (state: ReturnType<typeof fixture>) => { state.rulesets[0].rules[1].parameters.require_last_push_approval = false; },
  ]) { const state = fixture(); mutate(state); throws(() => validateReleasePolicy(state), /freshness|review/); }
});

test("malformed API responses fail closed", () => {
  for (const state of [null, {}, { ...fixture(), stablePolicies: null }, { ...fixture(), rulesets: [] }]) {
    throws(() => validateReleasePolicy(state), /docs\/release-automation\.md/);
  }
});
