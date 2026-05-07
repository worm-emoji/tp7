# Agent Notes

## Workflow

- Use conventional commit messages for commits.
- After each meaningful code change, run a smoke test before reporting completion.
- For the Rust CLI, the default smoke-test set is:
  - `cargo fmt -- --check`
  - `cargo check`
  - `cargo clippy -- -D warnings`
  - `cargo test`
  - at least one relevant `cargo run -- ...` command against the current feature.
- Keep the TP-7 CLI independent of installed companion apps. FieldKit and Dia may be used as research references only, not implementation dependencies.
