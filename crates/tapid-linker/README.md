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

The newer `plan_layout` API accepts `LayoutInput`: root dependency keys become
`<root>/node_modules/<name>`, while `DependencyEdge` entries are placed below
their parent's package directory (`.../<parent>/node_modules/<name>`). An
`InstanceKey` includes package identity plus peer and platform context, so
multiple versions and peer-context instances remain distinct. Conflicting
requests for one target are rejected rather than overwritten, and all planned
paths stay inside the managed root.

`plan_layout` selects `LinkKind::Symlink` on Unix and
`LinkKind::Junction` on Windows. Other platforms return an explicit
`UnsupportedPlatform` error. This is still a planner-only contract: no
filesystem mutation is performed.

Paths are lexical and deterministic; consumers must still perform their own
runtime checks before applying a plan.
