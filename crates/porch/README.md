# porch

Local git gate. Consent is `git push porch`. Independent review, cheap certify, then a GitHub PR. `origin` is never hijacked.

```sh
cargo install porch --locked
cargo install porch-quality --locked   # optional review engine
export PATH="$HOME/.cargo/bin:$PATH"
porch setup
porch doctor
```

Needs Rust 1.85+ and git. Full guide: [docs/install.md](https://github.com/jayden-dang/porch/blob/main/docs/install.md).

Apache-2.0.
