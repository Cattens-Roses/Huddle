# Contributing to Huddle

Thanks for helping improve this native Matrix desktop client.

By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).

## Ways to contribute

- **Bugs and UX issues** - open a [bug report](https://github.com/Cattens-Roses/Huddle/issues/new/choose)
- **Ideas** - open a [feature request](https://github.com/Cattens-Roses/Huddle/issues/new/choose)
- **Code** - fix issues or polish the desktop UI (Rust / GPUI)
- **Security** - report privately via [SECURITY.md](SECURITY.md); do not open a public issue for vulnerabilities

## Development setup

Requires the Rust toolchain pinned in `rust-toolchain.toml`.

```sh
cargo run -p baseapp
cargo check -p baseapp
cargo test -p baseapp matrix::
```

Keep the app focused on its reusable desktop shell and Appearance settings.
Prefer existing components and theme tokens from `crates/ui`, and keep
user-facing text clear and concise.

## Pull requests

1. Fork and branch from `main`.
2. Prefer small, reviewable PRs over large mixed changes.
3. Fill out the pull request template.
4. Ensure `cargo check -p baseapp` passes; add or extend tests when practical.
5. Do not commit secrets (`.env`, tokens, credentials).

## Issue guidelines

- Search existing issues before opening a new one.
- Include OS, app version (or commit), homeserver details if relevant, and steps to reproduce when reporting bugs.
- Feature requests should describe the problem and a concrete proposal.

## License

Contributions are licensed under GPL-3.0-or-later. See [COPYING](COPYING) and
[THIRD-PARTY.md](THIRD-PARTY.md).
