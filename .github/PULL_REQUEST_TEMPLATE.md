<!-- For anything bigger than a fix, please open an issue first. See CONTRIBUTING.md. -->

## What and why


## How it was checked

<!-- Tests added, and what you saw in the TUI (the tmux recipe in CONTRIBUTING.md). -->


## Tools used

<!-- Coding agents or other tools that wrote part of this, and how much; "none" is fine. -->


- [ ] `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test` pass
- [ ] Changed screen snapshots (`src/ui/snapshots/`) reviewed
- [ ] docs/architecture.md updated if files or structure changed
- [ ] docs/guide.md updated, and a file added under changelog.d/, if users would notice the change
- [ ] Mock names only (`ENG-52`, `acme`) in code, tests and docs
