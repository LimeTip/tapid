# tapid-runner

Planning and validation contract for policy-aware package scripts. A `RunnerRequest` carries the exact artifact digest and script; approvals bind to the exact digest and SHA-256 hash of the normalized script (CRLF/CR normalized to LF and outer whitespace trimmed).

The crate does not execute processes or provide sandboxing/containment. It reports explicit unsupported-OS limitations and validates approvals before any future execution layer is called.
