# Windows isolated Node execution

1. Reproduce real Node startup on Windows 11 VM126 as interactive limited `tapidtest`, with pinned artifacts. Separate stale probe results from this branch.
2. Reduce startup failure and test one variable at a time (memory limit, inherited stdio, path grants/environment); no speculative production edits.
3. Add a failing real-Node regression at the demonstrated seam, implement only the proven fix, retain read-only/no-network fail-closed scope.
4. Cross-build and run native regression plus argv/env/exit, denied read/write/network, child/timeout/Ctrl+C cleanup and exact project DACL restoration. Commit locally; no CI changes, push, or publication.
