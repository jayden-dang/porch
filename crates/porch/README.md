# porch

Local git gate. Consent is `git push porch`. Independent review, cheap certify, then a GitHub PR. `origin` is never hijacked.

`cargo install porch` installs **both** `porch` and `porch-quality`.

```sh
cargo install porch --locked
export PATH="$HOME/.cargo/bin:$PATH"
porch setup
porch doctor
```

Needs Rust 1.85+ and git. [Install](https://github.com/jayden-dang/porch/blob/main/docs/install.md) · [Usage A–Z](https://github.com/jayden-dang/porch/blob/main/docs/usage.md).

Apache-2.0.
