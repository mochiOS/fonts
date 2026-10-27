# UsuFont

Just a font library featuring Inter, IBM Plex, and UDEV Gothic. I’m truly grateful to the creators of these fonts.

## Runtime catalogue and management

This repository also provides the `mochios-fonts` Rust crate shared by AppCore
and ViewKit. `Locations` defines system/shared roots and an optional per-user
managed folder. `catalogue` discovers bounded SFNT/collection metadata without
retaining all font bytes. It rejects symlinks and reports unreadable/skipped files.
Discovery is bounded to 1,024 font files, depth 8, 512 MiB of input and 64 faces
per file; individual files are limited to 64 MiB. User roots are visited first.

`install`, `set_enabled` and `remove` only manage content-addressed, unchanged,
regular files directly inside the configured user folder. Installation uses
no-replace publication; disabling retains the file with a `.disabled` suffix.
System/shared and externally managed files are observations only. The caller
must perform mutations on a worker and provide removal confirmation. Native
capability and POSIX filesystem checks remain authoritative.

The native default user folder is `$HOME/.local/share/mochios/fonts`; Linux
preview uses `MOCHIOS_PREVIEW_CONFIG_ROOT/fonts` or an isolated preview home.
No user-write location is inferred from the host's real font directories.
`revision` supplies a small cached change token, and exact preview references
validate the allowed path, file digest and collection index before returning data.

`FontPreview::open` and `from_bytes` accept ordinary files and delegated document
bytes respectively. They validate before creating a disabled snapshot in the
reserved `.previews` directory, which discovery skips. The snapshot supports the
same exact-file alias across processes and is deleted when its last owner drops.
`FontPreview::install` publishes those exact bytes, without reopening the source.
Preview storage refuses more than eight files or 128 MiB; crashes can leave
snapshots behind. Opening alone never changes the font revision or activates a font.

Run `cargo test --manifest-path libraries/fonts/Cargo.toml` from the workspace.
The original Makefile, fonts.conf and resource installer remain the font-asset
build pipeline; the Rust API does not download fonts or modify OS-owned fonts.
