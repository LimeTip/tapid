# tapid-linker

`tapid-linker` owns the pure planning contract for materializing verified package
instances into a project layout. It accepts registry-qualified identities,
peer/platform context, and verified content-addressed tree references, then
returns stable, sorted paths under an explicitly managed root.

The plan includes an ownership-marker path (`.tapid-managed`), instance
materialization entries, and staged activation steps. It does **not** create
symlinks or junctions, mutate the filesystem, execute package scripts, or
provide an OS/process sandbox. Those effects belong to a future privileged
adapter and the policy/runner boundaries. `PlatformCapabilities` reports these
limitations explicitly, including unsupported platforms.

Paths are lexical and deterministic; consumers must still perform their own
runtime checks before applying a plan.
