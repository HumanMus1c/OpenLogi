# vendor/ksni — StatusNotifierItem, vendored patch

Third-party source: the Unlicense `ksni` crate
(<https://github.com/iovxw/ksni>) at 0.3.7 (2026-10-20), © iovxw (see
`UNLICENSE`). The code stays byte-for-byte upstream except for the patch
below, so it remains diffable against future releases.

This directory is **not** a workspace member on purpose: it keeps the upstream
form and does not take the workspace lint table. Consumers reach it through the
root `Cargo.toml` `[patch.crates-io]` entry; only the agent's Linux tray uses
it.

## The patch

- `Tray::label` / `Tray::label_guide`, exposed as the `XAyatanaLabel` /
  `XAyatanaLabelGuide` properties on `org.kde.StatusNotifierItem`. Ubuntu's
  AppIndicator GNOME Shell extension renders `XAyatanaLabel` next to the tray
  icon — it shows no SNI tooltip at all.
- The `XAyatanaNewLabel` signal for libayatana-appindicator hosts.
- `PropertiesChanged` entries for both label properties. GNOME's extension
  repaints its label from these; the upstream PRs only emitted the signal,
  which that extension does not subscribe to.

Upstream PRs [#42](https://github.com/iovxw/ksni/pull/42) and
[#47](https://github.com/iovxw/ksni/pull/47) cover the same feature. Prefer
upstream once it ships: refresh this directory from the release, re-apply
nothing, and delete the `[patch.crates-io]` entry.

## Rule for edits

Only the label patch may differ from upstream. Anything else belongs in
`crates/openlogi-agent/src/tray_linux.rs` (the consumer), not here. When
updating, diff against the published crate source and keep the patch minimal.

## Attribution

`UNLICENSE`, `license = "Unlicense"`, and the provenance comment at the top of
`Cargo.toml` (repository, version, author) stay. This is a legal fact about the
code's origin, not a style preference.
