## Summary

- **Process table:** keep the context menu open until an action is chosen and key it by pid; read RSS and owner in the cheap pass and resolve the user from the uid; users with no passwd entry sort last in both directions; strip control characters and bidirectional overrides from display names, falling back to `?` when nothing remains.
- **Sampler:** convert the disk busy window from microseconds to milliseconds before computing utilization; keep detail requests across ticks (selection every tick, the rest rotated); the detail queue, which grew without a bound, becomes a latest-wins slot; move disk, NIC, root-disk, and cpuinfo reads into the sampler; batch netlink dumps across interfaces in read_identities.
- **ZFS:** read the ARC minimum from `c_min` and use the saturating `reclaimable()`.
- **AppImage:** leave libxkbcommon on the host and drop any bundled copy. A new `host-compat.sh` apprun hook clears `GTK_MODULES`, points `GIO_MODULE_DIR` at an empty directory, and sets `NO_AT_BRIDGE`. A workflow, run by hand, builds the AppImage and attaches it to a release tag.

## Test plan

- [x] `cargo test -p tmog-core`: 116 passed, including the new display-name sanitizer test
- [ ] Run the app and confirm the process context menu stays open and actions apply to the right pid
- [ ] Confirm disk utilization reads sensibly under load
- [ ] Confirm ARC memory figures look right on a host with ZFS
- [ ] Build the AppImage with `packaging/appimage/package.sh` and launch it on a host newer than Ubuntu 22.04, checking for gvfs and at-spi errors
- [ ] Trigger the AppImage workflow against a test tag and confirm the asset is attached
