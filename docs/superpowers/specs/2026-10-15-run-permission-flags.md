# Portable invocation permissions

Issue: #285. Permission flags have identical names and meanings on Windows, macOS and Linux. Native backends continue to reject unsupported authority or assurance before execution. Standard Node remains unchanged.

Use repeatable `--allow-read <PATH>`, `--allow-write <PATH>` and `--allow-env <NAME>`, plus boolean `--allow-subprocess` and `--allow-network` (`--allow-net` alias). Paths use the existing portable project-relative syntax and resolve relative to the policy project root, including workspace invocations. Environment flags contain names, never values. Arguments after `--` remain opaque child arguments. Network permission means the existing unrestricted network boolean, not host/port filtering.

Without invocation permission flags, preserve existing exact-profile selection, defaults, assurance, limits and errors. With flags, support execution without editing or creating tapid.toml. An absent profile/default policy uses Required/Restricted assurance with no project filesystem, network, environment or subprocess grants. Runtime grants remain explicit backend evidence. Read, write, environment and subprocess authority must be requested separately. Existing configured defaults/profile restrictions remain a ceiling; supplied grant lists select narrower grants in that dimension, and omitted dimensions retain the configured policy. Preserve configured assurance and resource ceilings. Never silently replace ManagedTree with Restricted.

Resolve filesystem scope using the existing canonical grant resolver and grant kinds, including aliases, exact files and missing write directories. Do not implement string-prefix permission checks. Preserve existing trusted-host path race limitations and platform target exclusions. Reuse validated SandboxPolicy/FilesystemPolicy constructors and the existing request/runtime/environment/registry-credential checks. Configuration remains immutable.

Receipts retain their existing output channel and schema version, with additive input provenance for invocation-mode runs: configuration, invocation grants and effective policy, including environment names but no values. Effective enforcement still comes from checked backend evidence, not CLI assertions. Result-file transport remains #199.

Acceptance covers common parsing/help, no-profile runs, defaults/profile ceilings, repeated flags, invalid/escaping paths, canonical aliases and link attacks, reserved/credential environment names, opaque forwarded arguments, native allowed/denied operations and descendants, unsupported policies, immutable configuration and compatibility of profile-only callers. Native tests run on each claimed platform through CI; Windows local execution does not establish macOS/Linux native results.

The implementation uses profile ceilings to preserve configured denials and assurance. This keeps invocation permission selection separate from authorization to broaden a checked-in policy.
