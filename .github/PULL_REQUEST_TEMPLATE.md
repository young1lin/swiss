## What

## Why

## Gates

- [ ] `cargo test --workspace`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] database change: `cargo test -p swiss-it --features it` (or why it could not run)
- [ ] panel change: `npm run check` in `crates/swiss-panel/panel`, emit rebuilt and committed
- [ ] panel change: walked in a real browser on a fresh page load, English and 简体中文

## Notes for the reviewer

Sealed-format or wire changes (SPEC §formats), new dependencies (swiss-dependency-review), and
anything that moves idle memory (SPEC §product.memory) are called out here explicitly.
