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
- When the TP-7 is connected and write-path behavior changes, prefer
  `scripts/hardware-smoke.sh` for the device smoke. It uses `/memo` by default;
  override with `TP7_SMOKE_REMOTE_DIR=/existing/folder` if needed.
- Hardware smokes must use small generated files. Do not pull or rewrite
  existing TP-7 recordings unless the user explicitly asks; run `tp7 stat`
  first when touching any existing remote file so large notes are identified
  before transfer.
- Keep the TP-7 CLI independent of installed companion apps. FieldKit and other companion apps may be used as research references only, not implementation dependencies.
- Keep `docs/spec.md` and `docs/tp7-handshake.md` updated whenever protocol findings, command behavior, supported workflows, or reverse-engineering notes change.
- Keep CLI behavior aligned with https://clig.dev/: concise human-readable output by default, structured `--json` on stdout, diagnostics/errors on stderr, meaningful non-zero exit codes, helpful `--help`, and plain error messages that suggest the next command when possible.
