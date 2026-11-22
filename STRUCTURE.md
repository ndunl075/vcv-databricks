# Project source layout

The crate-level module names remain stable; `src/main.rs` maps them to the
organized source directories with Rust `#[path]` attributes.

- `src/ingest/` — event, link, and parsing input paths
- `src/indexing/` — column/index support
- `src/keys/` — key representations and key records
- `src/output/` — output writer
- `src/records/` — core and complex record models
- `src/runtime/` — runtime interface, bootstrap, system, and work queue
- `src/generic/` — generic processing implementation

This is a source-layout refactor. Existing crate-level module names and
application logic are intentionally retained.
