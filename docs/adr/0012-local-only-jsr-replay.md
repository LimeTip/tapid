# Local-only replay for graphs containing JSR

Frozen replay of a lock containing a logical JSR source requires every locked
tree locally verified before hydration can begin, including npm dependencies.
Offline replay already requires local verified content. Reuse verified-store
digest checks and the existing replay paths; retain npm-only frozen hydration.

This deliberately narrows the general frozen hydration contract to make mixed
JSR replay network-free as one unit. A missing or corrupt tree fails before any
artifact fetch or publication, rather than allowing an earlier npm dependency
to hydrate before discovering a missing JSR tree. The tradeoff is that users
must retain a fully warm verified store for JSR replay. JSR installation remains
experimental until end-to-end live installation and replay are verified.
