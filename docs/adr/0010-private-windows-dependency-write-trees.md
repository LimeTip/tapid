# Private Windows dependency write trees

The AppContainer token has low integrity, so DACL grants alone cannot make ordinary host-created files writable. Prepare only freshly copied private dependency write targets with low mandatory labels and inheritable no-write-up labels on directories. Validate every declared write object again before launch, reject reparse points and file hard links, and preserve the owner and DACL during preparation. Never lower the integrity of developer sources or shared store trees.

This extends the existing private BuildStage and AppContainer grant machinery. It does not make ordinary Windows project writes eligible, enable networking, weaken ManagedTree, or establish contained dependency support without native CLI acceptance and toolchain identity. Native owner-only lifecycle keys use a protected DACL and held non-inheritable handles, matching the Unix key contract.
