# Security posture of the dependency tree

Audited 2026-11-05 with `cargo audit` + `cargo deny check advisories`
(the GitHub Dependabot "moderate" alert was one of these).

## Unsound: glib (RUSTSEC-2024-0429) — ignored, unreachable

**What:** `glib::VariantStrIter`'s `Iterator` impls passed a C out-argument
behind `&`, which optimizations could drop → NULL deref/UB. Advisory:
https://rustsec.org/advisories/RUSTSEC-2024-0429

**Reachability in rsTMOG:** none. glib arrives via the Linux system-tray
stack (`tray-icon` → `libappindicator`/`muda` → `gtk` → `glib`) and our own
direct `gtk`/`glib` deps. The only glib calls in the tree we make are
`gtk::init`, `glib::timeout_add_local` and `gtk::main` (`src/traythread.rs`);
`glib::Variant` / `VariantStrIter` is used nowhere. The vulnerable code is
never executed.

**Why not upgraded:** no patched 0.18.x exists (fix landed in glib ≥ 0.20 /
0.22 lines), and `tray-icon` 0.20 and `muda`'s `gtk3` feature both pin the
gtk-0.18 stack. A clean move would mean switching `tray-icon` to its `ksni`
(D-Bus StatusNotifier) backend and dropping our direct gtk dep — viable, but
a tray-behavior change worth its own ticket, not a drive-by fix.
`cargo audit` still reports it; the ignore lives only in `deny.toml`
(`cargo deny check`). Note: `Cargo.lock` is gitignored for this bin crate,
so lockfile fixes below are local only.

**Action:** ignored in `deny.toml` with a full rationale. Revisit if we ever
touch `glib::Variant` APIs, or do the ksni migration.

## Unmaintained (warnings) — async-std / zbus / zvariant chains

`async-std` (discontinued), `pin-utils`, `proc-macro-error`, `ttf-parser`,
`zbus_names`, `zvariant`, `zvariant_derive` carry unmaintained advisories.
The zbus/zvariant/async-std chains come transitively from `dark-light`
(OS dark-mode detection). `cargo audit` treats unmaintained as warnings
(we have no `audit.toml` denying them); nothing actionable without replacing
`dark-light` (its 3.0 still depends on `zbus`, though it drops async-std).

## Yanked: async-executor 1.14.0

Fixed locally: `cargo update -p async-executor` → 1.14.1 (2026-11-05).
`Cargo.lock` is gitignored, so this lands via rebuilds; CI/build hosts pick
1.14.1 from the registry automatically.
