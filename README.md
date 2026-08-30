# porch

Local git gate. Independent review. Push only what survived.

```
git push porch
```

Porch is an **inner gate**: opt-in, local, isolated. You push to a remote named `porch` instead of `origin`. A disposable worktree rebases, reviews, and runs cheap local checks. Only then is the branch forwarded and a PR opened. `origin` is never hijacked.

## Install

```sh
cargo install porch --locked
export PATH="$HOME/.cargo/bin:$PATH"
porch setup
porch doctor
```

That one command installs **`porch` and `porch-quality`**. Full guide: **[docs/install.md](docs/install.md)**. Rust 1.85+ and git.

## License

Apache-2.0. Copyright 2026 The Porch Authors.
