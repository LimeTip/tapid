# Tapid development rules

The adoption path is the first product priority. Every change should preserve a working path for:

```text
tapid init
tapid i <package>
tapid install <package>
tapid install
tapid run dev
```

## Rust module structure

Rust source is split by responsibility, not by arbitrary line count:

- `main.rs`: binary entry point and process exit conversion only.
- `commands/`: Clap command definitions and one module per user-facing command.
- `application/`: orchestration across manifest, registry, resolver, store and linker crates.
- `filesystem/`: safe project path handling and transactional file operations.
- `output/`: stable human and machine-readable output.
- Library crates: domain types, parsing, validation and reusable behavior. They must not depend on CLI output or process termination.

Prefer a directory module with `mod.rs` only when a module has a meaningful internal hierarchy. Otherwise use `commands.rs` plus sibling files. Keep public interfaces small and deep, with implementation details private.

A file should normally have one responsibility. Split a file when it contains multiple independently testable concerns, when a section has its own error model, or when navigation requires substantial scrolling. Line count is a signal, not a hard rule. Avoid splitting a cohesive algorithm merely to meet a number.

## Change discipline

1. Add a focused test at the CLI boundary first.
2. Run the test and observe the failure.
3. Implement the smallest complete vertical slice.
4. Keep manifest semantics in `tapid-manifest`, registry transport in `tapid-registry-client`, resolution in `tapid-resolver`, storage in `tapid-store`, and materialization in `tapid-linker`.
5. Use runtime-derived temporary paths in tests.
6. Preserve existing user files and fail closed on malformed metadata or unverified artifacts.
7. Run focused tests, formatting, Clippy with warnings denied, workspace tests, and packaging before calling the change complete.

Do not add speculative commands, registry publication, sandbox claims, or authentication shortcuts as part of adoption work.
