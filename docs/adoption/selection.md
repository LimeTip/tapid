# Pilot selection

Uptime Kuma 1.23.16 is the first candidate: independently maintained frontend and Node service with a native SQLite dependency. Complete its journey before choosing the other four projects; no other project is counted as attempted. This historical snapshot is not a current-version recommendation.

Fresh static qualification on 2026-10-28 resolved the public tag to `5bb329fa0e8af9395911f33d5a6d6bb9b128c0f7`. The source archive, original manifest, npm v3 lock and retained MIT license were hashed. The SQLite archive matched the lock's SHA-512 integrity, and its actual manifest declares `node-pre-gyp install --fallback-to-build`. This establishes hook dependence, not permission to execute it.

See [pinned plan](uptime-kuma/plan.json) and [license](uptime-kuma/LICENSE). Acquisition used three public HTTP downloads (tag metadata, source archive, SQLite archive); subprocesses and local inspection calls are not included in this download count. Calls were not made through the capture runner; acquisition duration is unmeasured. A second, instrumented qualification run has [reviewed receipts](uptime-kuma/qualification/receipts.json): five launches, including three downloads, SHA-512 verification and hook-manifest inspection. All five passed; summed command duration was 1,374 ms (1,260 ms for downloads). The original three exploratory downloads remain separate; total exploratory call duration is unmeasured. Neither run is a full application journey.

Host: native macOS arm64, Node 22.23.3; source checkout `99c2bffa2418cc49551db8ce64fe82c9c479b1ca` before these uncommitted changes. `docker info --format '{{.OSType}}/{{.Architecture}}'` failed because no daemon socket was present. No disposable execution environment or qualified Linux lifecycle host is established. No external npm code was executed on the credential-bearing host. Baseline, import, native install, add/update, replay and rollback remain not reached, rather than failed application evaluations. No Tapid binary identity or npm baseline is claimed.

Reproduce acquisition without executing downloaded code:

```sh
curl -fLsS https://api.github.com/repos/louislam/uptime-kuma/git/ref/tags/1.23.16 -o tag.json
curl -fLsS https://codeload.github.com/louislam/uptime-kuma/tar.gz/5bb329fa0e8af9395911f33d5a6d6bb9b128c0f7 -o source.tar.gz
curl -fLsS https://registry.npmjs.org/@louislam/sqlite3/-/sqlite3-15.1.6.tgz -o sqlite.tgz
```

Verify all identities against the plan before extracting or running anything. Preserve original source inputs. npm v3 conversion is unnecessary for this pinned lock. Inspect backend-test service requirements and server binding/readiness before execution; the `npm test` preparation script deletes `data/test`; the backend suite imports server/database modules and must not be assumed hermetic. The plan selects `is-odd@3.0.1` for addition and `dayjs` 1.11.13 → 1.11.14 for a bounded named update. Three further exploratory metadata downloads qualified these targets (including the existing Day.js version); durations were not instrumented. The update requires a deliberate exact manifest declaration because the CLI accepts names rather than version arguments. Record that manual change separately from unchanged-manifest maintenance. Metadata identity is not archive verification or hook authority. Build/publish/deployment scripts in the source are not authorized evaluation commands.
