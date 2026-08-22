# tapid-archive

`tapid-archive` validates archive entry metadata before extraction. Names using either slash style are checked. Traversal, absolute, drive, and UNC paths are rejected. Entry count, path length, and byte limits are explicit. Duplicate and case-colliding names, hardlinks, device nodes, and other special files are rejected. Relative symlinks are allowed only when their lexical target remains inside the archive root.

The crate does not decompress archives, access the filesystem, scan executable content, or provide malware detection. Callers provide typed entry metadata and choose limits explicitly. Extraction and store activation belong to other layers.
