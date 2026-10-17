# Windows administrator-installed network guard

## Authorization and outcome

On October 17 the user explicitly requested building the administrator-installed guard and clarified that native acceptance must use this Windows device. Do not require the historical VM126 target. Record actual Windows edition/build, architecture and runtime. Installation is a one-time administrator operation; ordinary `tapid run` must remain unattended. No commit or push is authorized for this follow-up.

## Boundary

A LocalSystem Windows service can add only deny filters for a verified caller-owned AppContainer/Job execution. It cannot launch commands, grant network authority, read project paths, accept arbitrary filter descriptions, or transfer privileged handles/tokens to a caller. The existing standard-user socket/DNS broker continues to own network=true. The guard owns network=false and covers native calls and descendants independently of compatibility hooks.

Windows Filtering Platform ALE_RESOURCE_ASSIGNMENT_V4/V6 support ALE_PACKAGE_ID conditions. The service installs two persistent BLOCK filters matching the exact per-execution package SID. This denies explicit and implicit binds. Zero network capability SIDs and ordinary AppContainer network controls remain in place. No firewall exemptions, shared package identity, new capability SIDs or broad user/system filters are allowed.

## Public workflow and installation

Add `tapid sandbox install`, `tapid sandbox status`, `tapid sandbox uninstall`, with explicit Windows-specific help and an unsupported-platform diagnostic elsewhere. The CLI composes runner-owned operations. The existing first-in-main private-launcher dispatch recognizes an exact private service invocation; main.rs gains no domain behavior. No additional published executable or crate dependency is required.

Install a byte-verified copy of the current Tapid executable in a fixed OS-known Program Files directory with a protected Administrators/SYSTEM-write, Users-read/execute DACL. Reject unexpected existing installations/reparse points; never execute a service image from a worktree or user-writable cache. Register TapidNetworkGuardV1 as an auto-start LocalSystem service dependent on BFE, using a quoted protected image path and exact private argument. Do not automatically elevate routine runs. Installation reports a direct administrator-required diagnostic when not elevated; native setup is the final reviewable action through one UAC prompt.

Uninstall is explicit and administrator-only. Refuse while execution leases are active. Stop the service gracefully, then remove the service registration and owned image. Never remove orphan deny filters merely because a process/service disappeared. Status reports installation/running state and explains that an unavailable guard makes no-network executions fail closed.

## Authentication and bounded IPC

The fixed local named pipe is created with FIRST_PIPE_INSTANCE by the service, rejects remote clients, and has a protected DACL granting SYSTEM/Admin full access and interactive clients only data read/write and required attributes/synchronization. Do not grant FILE_CREATE_PIPE_INSTANCE through generic-write access. Clients open only those precise rights, use identification-level SQOS, and verify the server's held process token is LocalSystem and corroborate its PID against the running service's protected image and SCM configuration before accepting a response.

The service derives client PID from the pipe, holds that process, reads a complete fixed registration frame, impersonates the pipe only long enough to inspect identity, and reverts on every path. Compare the peer identity with the held caller process's primary user, logon LUID, session, integrity, elevation type, restriction and AppContainer state; reject AppContainer and below-medium callers. Clone the caller's supplied root-process and Job handles with SAME_ACCESS, never upgrading access. Query granted access and require Job QUERY+TERMINATE rights already present in the source handle. Validate object types via their actual APIs, root user equality, AppContainer state, exact package SID, zero capabilities and Job membership. Derive the filter SID from the held target token, not caller bytes. A caller may only constrain its own execution; a Job must already grant termination to that caller before the service may use it.

Frames carry version, operation, held-handle values and zero reserved bytes; no PID, SID, path, command, host/port or filter key is caller selectable. Bounds: 256 connections, 64 active leases per user, fixed <=64-byte frames, two-second incomplete-frame deadline, bounded nonblocking reads/writes and bounded client handshake. Service status and finish operations accept no extra parameters. The kernel objects, not numeric identifiers alone, bind each connection.

## Lifetime and failure ordering

Register after the suspended root is assigned to its private Job and before compatibility installation or project resume. A successful response requires both deny filters to commit atomically. A missing, wrong-identity, busy, incompatible or failed guard tears down the suspended tree and fails with no script marker or enforcement receipt.

Filters are PERSISTENT in a persistent service-owned provider/sublayer, not dynamic-session filters. A service crash must not automatically remove denial while a child lives. Keys derive from package SID plus layer; duplicate registration must not delete an existing lease's filters. Count outstanding owned filters and bound publication; exhaustion fails closed.

The service holds the caller process and already-authorized Job. On caller exit/disconnect it terminates that Job and keeps denial until the kernel's active-process count is zero. On normal FINISH it likewise verifies zero before deleting exactly its two filters. Failed proof/removal retains filters and reports failure; no elapsed time is treated as emptiness. On ordinary service stop, stop accepting new work, terminate/drain owned Jobs, remove only proven-safe filters, then stop. Unexpected crash may leave orphan deny filters; report them and retain them rather than guessing that descendants exited. Administrative host tampering/BFE replacement remains a trusted-host boundary.

The parent retains its guard channel through verified-empty-Job cleanup and requires the service cleanup acknowledgement before successful outcome. The mechanism/limitations disclose the guard. Missing guard is not silently accepted as full network denial. Tests exercising the production no-network path require an installed guard; hosted Windows setup installs it once before native tests and uninstalls the idle guard afterward.

## Structure and reuse

All enforcement, service lifecycle, authentication and installation live privately in tapid-runner/windows_guard, with one narrow management interface re-exported for CLI composition. Reuse standard OwnedHandle, existing suspended-child/Job cleanup, token-query patterns and byte verification. Keep the existing socket/DNS broker's different grant/delegation contract separate from the privileged deny service. No generic broker framework or miscellaneous utility module.

## Proof obligations

- Wire malformed/reserved/oversized/partial input, wrong objects, insufficient source Job rights, wrong user/SID/Job, spoofed non-SYSTEM server, AppContainer caller and unavailable service all fail closed.
- Elevation is needed only for install/uninstall. Standard-user actual CLI and Rust controls deny native IPv4/IPv6 TCP/UDP bind/connect, including descendants, while host and network=true positive controls still work.
- No-network project code cannot remove filters or contact the privileged pipe. Normal completion, timeout, Ctrl+C, parent disconnect and ordinary preparation failures remove filters only after Job emptiness.
- Unexpected service termination leaves persistent denial. Restart does not silently remove orphans. Filter/provider counts and diagnostic state remain bounded.
- Run current-device original news-site install/replay/build/test/start/readiness and actual CLI receipt checks. Record Restricted versus ManagedTree accurately. Run affected tests, Clippy, formatting, nested integration and the full local lane once; retain unrelated symlink-privilege failures honestly.

References: https://learn.microsoft.com/en-us/windows/win32/fwp/filtering-conditions-available-at-each-filtering-layer ; https://learn.microsoft.com/en-us/windows/win32/fwp/object-management ; https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-impersonatenamedpipeclient
