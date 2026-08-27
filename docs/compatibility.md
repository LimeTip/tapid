# Compatibility matrix

This matrix describes the current implemented contract. Supported means covered by local contract or crate tests. It does not mean full npm compatibility or production readiness.

| Contract | Current behavior | Stable limitation |
|---|---|---|
| Manifest | Parses selected npm-shaped fields, dependency maps, scripts, and string or object `bin` metadata | Arbitrary npm extensions, workspace metadata, and lifecycle semantics are not implemented |
| npm registry | Reads npm `versions`, validates package/version identity and HTTPS tarballs, and requires a valid SHA-512 `dist.integrity` by default; `tapid install --allow-unverified-registry-artifacts` is an explicit interactive compatibility escape hatch | The escape hatch is not allowed with `--offline` or `--frozen`, emits a warning, and does not provide registry-declared artifact authentication; aliases, tags, git, file, workspace specs, private authentication, and complete packument behavior are unsupported |
| JSR registry | Accepts scoped metadata and semver versions; accepts an artifact only with explicit HTTPS `npm.tarball` and valid SHA-512 `npm.integrity` | Live JSR installation and integrity behavior are not verified; no derived or transport-only integrity is accepted |
| Resolution | Deterministic graph selection for exact, caret, tilde, and selected comparison requirements | Full npm semver, prereleases, unions, tags, aliases, peers, optionals, and platform-specific resolution are incomplete |
| Online install | Resolves npm metadata, downloads and verifies archives, writes `tapid.lock`, stores verified trees, and atomically activates `node_modules` | Lifecycle scripts never run; update, add, remove, prune, workspaces, and full npm lockfile behavior are absent. The compatibility escape hatch records the locally computed archive integrity, but the current lockfile format does not persist whether the registry declared that integrity; frozen replay therefore cannot distinguish an originally unverified registry artifact. |
| Offline and frozen install | Requires a lockfile, matching root manifest digest, verified store trees, and valid `.tapid-tree` markers; performs no network work | Frozen currently selects the same replay path as offline and is not complete npm frozen-lockfile policy |
| `.bin` | Package `bin` metadata is planned and materialized from verified regular files; Unix symlinks and Windows `.cmd` plus PowerShell wrappers | Other platforms are unsupported; collisions, unsafe targets, and missing targets fail closed |
| Root scripts | `tapid run` executes an explicit root script in the project directory, prepends managed `.bin`, forwards `--` arguments, and returns child status | Platform shell execution is not a sandbox and root scripts can run arbitrary code |
| Archive | Bounded hostile-path, duplicate, case-collision, symlink, and special-file validation | Decompression, malware scanning, and executable analysis are outside the crate |
| Store and lockfile | SHA-256 content-addressed staging, exact tree replay, canonical lockfile v3, and atomic managed activation | No remote cache, garbage collection, lease protocol, or full npm lockfile graph |
| Platforms | macOS behavior is locally exercisable; Linux and Windows consumer checks are configured in GitHub Actions | Configured CI is not execution evidence until a workflow run completes; macOS is not Linux or Windows evidence |

Platform contexts preserve independent OS, CPU, and libc fields. Empty contexts are encoded as `platform=-`; non-empty partial contexts use fixed slots, for example `linux--` for OS-only and `-x86_64-` for CPU-only. Legacy flattened one- and two-component values are rejected because their field positions are ambiguous.

Local fixtures are runtime-derived and do not imply live registry or cross-platform validation. The CI workflow is the authoritative place for configured Linux and Windows consumer checks.
