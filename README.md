# rsTMOG

Task Manager OG, rewritten in Rust. Linux-only by design.

A from-scratch Rust reimplementation of the TMOG concept (Dave Plummer's
Task Manager OG), inspired by the behavioral design of
[evildojo-tmog](https://github.com/mikedesu/evildojo-tmog) (MIT, Michael
Bell), which remains available locally as a read-only reference at
`~/repos/evildojo-tmog`. No C++ code is ported line-by-line; the Rust
core is written idiomatically.

## Layout (planned)

- `src/core/` — metrics collectors over `/proc`, `/sys`, NVML (pure parsers, unit-tested against fixtures, no GUI deps)
- `src/gui/` — egui/eframe frontend (Summary, CPU, Memory, Disks, Network, Thermals, Processes, System pages)
- `tests/fixtures/` — captured procfs trees for collector tests

## Building

```sh
cargo build          # debug
cargo run            # GUI
cargo nextest run    # test suite
cargo clippy --all-targets -- -D warnings
cargo fmt --check
```

Requires a working Rust stable toolchain (`rustup`). GPU support dlopens
`libnvidia-ml.so` at runtime — no build-time NVIDIA dependency.

## Remotes

- `origin` — Forgejo: https://www.lm9llm.com/github/jcain/rsTMOG (private)
- `github` — https://github.com/jcgit1/rsTMOG (private)

Push both: `git push origin main && git push github main`

## License

MIT. "Task Manager OG"/TMOG is Dave Plummer's product; this project is
not affiliated with or endorsed by him.
