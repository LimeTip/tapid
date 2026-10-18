# Recommended engineering skills

Use the skills below when installed and their trigger matches the task. Read the selected skill's complete `SKILL.md` before following it, then load supporting references as needed. Locate skills through the runtime's skill catalog; for Codex, these skills are installed under `~/.codex/skills/<name>/`.

Sources: [Matt Pocock's skills](https://github.com/mattpocock/skills) and [HumanLayer's show-me](https://github.com/humanlayer/skills/tree/main/plugins/show-me/skills/show-me). This guide recommends workflows; it does not install skills for contributors.

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

Reuse existing repository conventions and equivalent skills. For example, coordinate `tdd` with `test-driven-development` as one red-green-refactor workflow rather than running duplicate processes. When an installed skill is missing from the catalog, read its known installed path if accessible; if unavailable, explain the missing prerequisite and continue work that does not depend on it.

Read existing domain documents before creating new ones. A repository's `CONTEXT.md` may contain behavior contracts beyond a glossary; assess its contents and references before proposing a move to `GLOSSARY.md`.
