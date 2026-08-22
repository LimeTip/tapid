# tapid-archive

`tapid-archive` validates archive metadata before any extraction. It treats archive names as hostile input: both slash styles are checked, traversal and absolute/drive/UNC paths are rejected, entries are bounded by count/path/size limits, duplicate and case-colliding names are rejected, and hardlinks, device nodes, and other special files are not accepted. Relative symlinks are allowed only when their lexical target remains inside the archive root.

The crate is deliberately independent from the local store and does not access the filesystem; callers provide typed entry metadata and choose limits explicitly.
