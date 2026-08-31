# tapid-archive


[Crates.io](https://crates.io/crates/tapid-archive) | [GitHub](https://github.com/LimeTip/tapid/tree/main/crates/tapid-archive)

`tapid-archive` validates archive entry metadata before extraction. Names using either slash style are checked. Traversal, absolute, drive, and UNC paths are rejected. Entry count, path length, compressed size, extracted size, and path limits are explicit. Duplicate and case-colliding names, links, device nodes, and other special files are rejected. Tar and tar.gz archives are extracted into a fresh staging directory.

Unix extraction normalizes regular files to `0644` or `0755`, preserves only the executable distinction, and strips ownership and privilege bits. A canonical internal mode manifest makes executable-aware tree digests identical across Unix and Windows while Unix verification also checks the actual mode. The crate does not scan executable content or provide malware detection; store activation remains a separate layer.
