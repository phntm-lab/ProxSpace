## What this changes, and why

<!-- A paragraph. What was wrong or missing, and what this does about it. -->

## Checks

- [ ] `cargo test`
- [ ] `cargo fmt --check`
- [ ] `cargo clippy --all-targets -- -D warnings`
- [ ] `tests/layers.rs` passes, or the layers themselves were changed on purpose
- [ ] Behaviour changed → `CHANGELOG.md` has a line under the version in
      `Cargo.toml`, and a test that fails without this change
- [ ] Docs changed → both `README.md` and `README_RU.md`

Anything unticked is fine as long as it says why.
