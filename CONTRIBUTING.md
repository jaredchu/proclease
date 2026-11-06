# Contributing

Bug reports and pull requests are welcome. Start with [project context](docs/project-context.md)
and [project instructions](AGENTS.md). Contributions are provided under the
[MIT license](LICENSE).

For process issues, include the OS, ProcLease version, reproduction commands,
expected result and observed lifecycle state. Remove credentials and private
application data from reports and logs.

## Checks

Use a current stable Rust toolchain and `/usr/bin/python3`:

```sh
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked --release
```

Use isolated private state for process tests and stop their services. Keep build
outputs, test state and private diagnostics out of Git. See the
[verification record](docs/verification.md) for tested behavior and limits.

## Commit attribution

Keep the contributor's normal Git author identity. For work assisted by Codex,
include this trailer exactly once, separated from the commit body by a blank line:

```text
Co-authored-by: Codex <noreply@openai.com>
```

For every commit Claude helps implement, validate or review, include this trailer
exactly once:

```text
Co-authored-by: Claude <noreply@anthropic.com>
```

Credit completed validation and review even when Claude does not edit files.
When both Codex and Claude assist the work in a commit, include both trailers,
each exactly once. Preserve existing co-author trailers and credit only actual
assistance. Apply this to new commits; do not rewrite published history solely
for attribution. The README acknowledgment describes project roles and does not
promise a particular position in GitHub's Contributors graph.
