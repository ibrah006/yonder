# Contributing

Thanks for your interest. Yonder is not accepting outside code contributions yet: the
contribution terms (CLA or DCO) are still being decided, and we want them settled
before merging anyone else's work.

Bug reports and ideas are welcome as GitHub issues. For security problems, see
[SECURITY.md](SECURITY.md) — please don't open a public issue.

## Working on the code

```sh
cargo fmt --all
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

CI runs the same checks on Windows, macOS and Linux.
