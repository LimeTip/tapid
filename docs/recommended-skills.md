# Recommended engineering skills

Use the bundled skills below when their trigger matches the task. Read the selected skill's complete `.agents/skills/<name>/SKILL.md` before following it, then load supporting references as needed. These files travel with the checkout; no global installation is required.

The 14 recommended skills and the `grilling` dependency are vendored with their supporting files. [Source revisions and licenses](../.agents/skills/SOURCES.md) identify the unchanged upstream imports.

## Runtime discovery

| Runtime | Repository discovery | Using a skill |
| --- | --- | --- |
| Codex | `.agents/skills/<name>/SKILL.md` | Select the skill from the catalog or request it by name. |
| Claude Code | `.claude/skills/<name>/SKILL.md` | Invoke `/name` or allow automatic selection for model-invoked skills. `CLAUDE.md` imports `AGENTS.md` for shared repository rules. |
| Hermes | Current versions discover `.agents/skills` in a trusted project. | Run `hermes skills trust` once from the repository, then use `/name` or `skill_view`. Older versions without project discovery can read the canonical files directly through the guidance in `AGENTS.md`. |
| T3 Code | Discovery follows the selected provider: `.agents/skills` for Codex and `.claude/skills` for Claude. | Select the provider's project skill or request its name. Start the thread at the repository root; use the direct-file fallback if the provider does not expose it. Hermes availability depends on its installed version and trust configuration. |

These locations follow the [Codex documentation](https://learn.chatgpt.com/docs/build-skills), [Claude documentation](https://code.claude.com/docs/en/skills), [Hermes documentation](https://hermes-agent.nousresearch.com/docs/user-guide/features/skills), and [T3's Claude discovery implementation](https://github.com/pingdotgg/t3code/blob/main/apps/server/src/provider/Drivers/ClaudeSkills.ts).

Prefer the bundled copy when a global skill has the same name. Some runtimes give personal copies higher priority or list both copies; read the repository file explicitly to use this pinned version. Reload skills or start a fresh session after changing discovery directories if the runtime does not refresh them automatically. Trust and policy settings still apply.

When an upstream skill says to call a "Skill tool," use the runtime's native skill loader. If none is available, read the named skill's canonical `SKILL.md` and follow it, resolving reference paths relative to that skill directory. Use the runtime's actual tools for subagents, worktrees, and task tracking. In T3, follow its injected delegation instructions. If a required capability is unavailable, explain the limitation and agree an alternative rather than claiming the orchestration ran.

## Maintaining the bundle

Edit or update `.agents/skills` only. The committed `.claude/skills` tree contains regular files so fresh Windows checkouts work without symlink privileges. Regenerate it from the repository root:

```text
node --experimental-strip-types scripts/sync-agent-skills.ts --write
node --experimental-strip-types scripts/sync-agent-skills.ts --check
node --experimental-strip-types --test tests/agent_skills_test.ts
```

The check compares the complete trees, including references and licenses. Extra mirror files require manual review and removal; the writer does not delete files. The existing CI developer-test lane runs `tests/dev_test.ts`, which includes the packaging tests and checks the committed mirror. This verifies packaging and mirror consistency; it does not simulate every agent runtime.

## Skills to use when relevant

| Skill | When to use it | How to apply it |
| --- | --- | --- |
| `pr` | Writing or updating a pull request description. | Show the change, concrete before/after evidence, reversibility, and blast radius. Follow the repository's PR template when present. |
| `domain-modeling` | Resolving domain terminology or changing a glossary or ADR. | Check terms against code and existing domain docs, probe concrete edge cases, and record resolved terms and consequential decisions. |
| `codebase-design` | Designing a module interface, choosing a test seam, or improving testability. | Use its deep-module vocabulary to keep the public interface small and implementation private. |
| `tdd` | Implementing behavior or fixing a bug test-first. | Test through agreed public interfaces, one failing test and minimal implementation at a time. Reuse the repository's established test seams and verification commands. |
| `code-review` | Reviewing a branch or changes against a spec. | Establish the comparison base and spec, then review standards and spec compliance separately using the skill's reviewer workflow. |
| `writing-for-agents` | Editing agent instructions, skills, or reference docs for agents. | Write precise triggers and completion criteria; keep detailed reference material behind useful pointers. |

## Workflows to run on request

These skills declare `disable-model-invocation: true`. Run them when the user requests the skill or its workflow; otherwise recommend the appropriate one rather than launching it automatically.

| Skill | User's requested workflow | How to apply it |
| --- | --- | --- |
| `setup-matt-pocock-skills` | Configure a repository for these engineering workflows. | Inspect existing conventions, agree the issue tracker and domain-doc layout, and show the proposed configuration before writing it. |
| `grill-with-docs` | Interview the user about a plan while recording domain language and decisions. | Read `grilling` and `domain-modeling`; resolve decisions in rounds and capture settled terms and ADRs as appropriate. |
| `to-spec` | Turn an existing discussion into a specification. | Synthesize the agreed problem, behavior, decisions, scope, and test seams; use the configured destination. |
| `to-tickets` | Break a spec or plan into implementable tickets. | Propose complete vertical slices with explicit blockers, get agreement on the breakdown, and publish through the configured tracker. |
| `implement` | Implement a particular spec or ticket. | Read the source requirements, implement with `tdd`, and finish with `code-review` and the repository's required checks. |
| `implement-spec` | Implement an entire spec across its ticket dependency graph. | Use isolated implementer worktrees and an integration branch, work ready tickets concurrently, integrate completed work, and review the result. |
| `retro` | Review a coding session for improvements to the agent's environment. | Read actual session evidence and `writing-for-agents`, then present concrete improvement candidates by severity for human judgment. |
| `show-me` | Explain the current topic visually. | Choose the smallest useful diagram, pseudocode, diff, tree, or focused HTML artifact. |

For a new feature, the optional sequence is `grill-with-docs` -> `to-spec` -> `to-tickets` -> `implement` for one ticket or `implement-spec` for the whole graph. Use `pr` when preparing the PR description and `retro` when a retrospective is requested. Each request-driven stage needs the user's request; this sequence does not authorize running every stage.

## Integration with existing instructions

Repository instructions and the user's authorization take precedence over skill defaults. In particular, a skill's commit, push, publication, issue-tracker mutation, or cleanup step does not override an applicable approval requirement. Respect the runtime's delegation and worktree rules.

Reuse existing repository conventions and equivalent skills. For example, coordinate `tdd` with `test-driven-development` when the latter is installed as one red-green-refactor workflow rather than running duplicate processes. When a bundled skill is missing from the catalog, read its canonical path directly; if unavailable, explain the missing prerequisite and continue work that does not depend on it.

Read existing domain documents before creating new ones. A repository's `CONTEXT.md` may contain behavior contracts beyond a glossary; assess its contents and references before proposing a move to `GLOSSARY.md`.
