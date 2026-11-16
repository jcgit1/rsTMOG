# Security posture of the dependency tree

Audited 2026-11-05 with `cargo audit` + `cargo deny check advisories`
(the GitHub Dependabot "moderate" alert was one of these).

## Unsound: glib (RUSTSEC-2024-0429) — removed

**What:** `glib::VariantStrIter`'s `Iterator` impls passed a C out-argument
behind `&`, which optimizations could drop → NULL deref/UB. Vulnerable
range is glib 0.15.0 through 0.19.x. Advisory:
https://rustsec.org/advisories/RUSTSEC-2024-0429

**Resolution (2026-11-15):** the crate is gone from the tree. The Linux
tray now uses `tray-icon` 0.26 with the `ksni` feature (StatusNotifier
over the session bus) and no longer depends on `gtk` or `glib`. No
patched 0.18.x exists, and `gtk` 0.18 cannot take `glib` 0.20, so the
upgrade path was to leave that stack. `deny.toml` no longer ignores the
advisory.

## Unmaintained (warnings) — async-std / zbus / zvariant chains

`async-std` (discontinued), `pin-utils`, `proc-macro-error`, `ttf-parser`,
`zbus_names`, `zvariant`, `zvariant_derive` carry unmaintained advisories.
The zbus/zvariant/async-std chains come transitively from `dark-light`
(OS dark-mode detection). `cargo audit` treats unmaintained as warnings;
`deny.toml` sets `unmaintained = "workspace"`, so `cargo deny check` fails
only if a *direct* dependency becomes unmaintained (vulnerabilities always
fail). Nothing actionable without replacing `dark-light` (its 3.0 still
depends on `zbus`, though it drops async-std); the zbus chain also comes
from the ksni tray, and `ttf-parser` from egui.

## Yanked: async-executor 1.14.0

Fixed: `cargo update -p async-executor` → 1.14.1 (2026-11-05); the
committed `Cargo.lock` now pins it.

## Release build tools

linuxdeploy, appimagetool and the AppImage runtime are pinned to tagged
releases and SHA-256 verified before they run
(`packaging/appimage/tools.sh`); nothing is fetched from a `continuous`
tag.

## Release workflow

`.github/workflows/appimage.yml` builds the tagged source (and refuses a
tag that doesn't match `Cargo.toml`) in a job with a read-only token that
is not left in `.git/config`, so dependency build scripts can't write to
the repo. A separate publish job, which runs no project code, attaches the
files and never replaces existing assets. Actions are pinned to commit
SHAs.
