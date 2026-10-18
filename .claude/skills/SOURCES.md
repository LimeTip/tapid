# Vendored skill sources

`.agents/skills` is the canonical source. `.claude/skills` is a generated,
committed copy for Claude Code and T3's Claude provider. Keep all supporting
files and license notices when updating either upstream import.

| Upstream | Pinned commit | Imported paths | License |
| --- | --- | --- | --- |
| [mattpocock/skills](https://github.com/mattpocock/skills/tree/49dd158d1076134a641b33efb035946536778336) | `49dd158d1076134a641b33efb035946536778336` | `skills/engineering/{code-review,codebase-design,domain-modeling,grill-with-docs,implement,implement-spec,pr,retro,setup-matt-pocock-skills,tdd,to-spec,to-tickets}` and `skills/productivity/{grilling,writing-for-agents}` | [MIT](LICENSE.mattpocock) |
| [humanlayer/skills](https://github.com/humanlayer/skills/tree/653b6411c1f70c275a18e37673b042ff99f67ceb) | `653b6411c1f70c275a18e37673b042ff99f67ceb` | `plugins/show-me/skills/show-me` | [MIT](LICENSE.humanlayer) |

The imported skill files are unchanged from these revisions. `grilling` is
included because `grill-with-docs` invokes it. The root repository instructions
and [runtime guidance](../../docs/recommended-skills.md) explain how to apply
the upstream workflows with this repository's rules and the available tools.

To update, review the upstream changes, replace the affected canonical skill
directories and licenses, and update the revision above. Then run the mirror
command and checks documented in the runtime guidance. Edit only the canonical
tree; the mirror command reports extra files for manual review and removal.
