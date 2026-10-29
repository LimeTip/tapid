# Shared dependency interpretation in the resolver

Manifest declarations retain their section, original binding spelling, and raw
specification in `tapid-manifest`. Pure installation interpretation belongs to
`tapid-resolver`, which now depends on manifest rather than making manifest depend
on resolution. This concentrates workspace requirements, aliases, override rules,
source classification, root membership, and provider interpretation behind one
capability interface instead of coordinating independent CLI parsers. Expanding
`tapid-core` or introducing a general utility crate would obscure those owners;
the additional resolver compile dependency is the deliberate tradeoff.

Callers supply indexed workspace name/version facts and fallible registry selectors.
Interpretation retains local names separately from actual targets and exact registry
origins, preserves overlapping declarations, and performs no filesystem or network
operations. The CLI retains discovery, artifact preparation, lock-record assembly,
link planning, and transaction ordering. Existing copied-source parsers and lockfile
constructors remain authoritative; no source support or lock schema changes.

Replay consumes the same declaration view, membership rules, and requirement
interpretation without resolving the recorded graph again. Legacy reconstruction
retains declared identities and all applicable requirements without applying fresh
resolution overrides. Modern binding, recorded-edge, and ambiguity checks remain
replay contracts. Workspace registry peers retain their direct-root provider rule,
which differs from registry-package peers selected through local root bindings.
