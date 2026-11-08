# Fork change ledger

Scope: every commit unique to this fork before this ledger, i.e.
`upstream/master..ffff85ee` at 2026-11-02 (upstream `e581c205`): 85 commits — 16
already on `master`, 69 on `feat/tray-light-menu`. The docs commits that added
this ledger follow the range. Working-tree changes are not commits and are
outside this ledger.

This is a history, not a design document. Decisions, measurements, and the
per-surface verification logs stay in the owning plan
([battery-display.md](battery-display.md)); this file answers "what landed, why,
what proved it, and is it still there?".

Reading the entries:

- **Status** is relative to HEAD: `kept`, `superseded by <hash>`, or
  `reverted by <hash>`.
- **Verified** records only checks saved in the commit message or plan. "None
  recorded" means no evidence was saved, not that the change was untested.
- Entries are oldest first inside each theme; themes follow the first commit.
- Maintenance is defined in `AGENTS.md` (Git & GitHub): behavioural commits
  update their entry in the same commit; mechanical commits are exempt. A new
  entry starts with `pending` in place of its hash; the next docs commit fills
  it in.

| Theme | Commits |
|---|---:|
| Shared reading rules and settings | 2 |
| Tray surfaces on Windows and macOS | 4 |
| Flow Indicator card | 5 |
| Linux tray | 1 |
| Demo device source and mock tree | 10 |
| Tray readout breadth | 9 |
| Overlay appearance, lifecycle, and windows | 7 |
| Window placement memory | 2 |
| Pinned card and shadow | 31 |
| Documentation checkpoints | 11 |
| Style, chore, and mechanical | 3 |
| **Total** | **85** |

Nine behavioural commits also updated the plan in the same commit:
`3090ead7`, `338f81c2`, `3f9c2219`, `0eaae721`, `37662f8e`, `181a7d02`,
`df8f287d`, `fe92d9a5`, `ce2ffcc9`. Post-ledger commits are listed in §12;
they are outside the 85-commit count.

## 1. Shared reading rules and settings

- `b16cfcf5` **refactor(core): own the battery reading rules in BatteryInfo** — kept.
  Why: the desktop, tray, and card each derived charge state, the cold-start
  charging rule, and the tone from raw fields; a second threshold was inevitable.
  Fix: `BatteryReading`/`BatteryTone` and `BatteryInfo::reading`/`tone`/
  `bar_percentage` live in `openlogi-core`; an ast-grep guard rejects another copy.
  Verified: no live check recorded; the guard is the regression proof.
- `3090ead7` **feat(core): add the tray and Flow Indicator battery-readout settings** — kept.
  Why: both surfaces needed switches and limits without a config schema bump.
  Fix: additive `[app_settings]` fields (`tray_battery`, `flow_indicator`,
  `flow_indicator_show_offline`, `flow_indicator_max_devices` clamped 1–5,
  `flow_indicator_display_mode`), all serde-defaulted; the plan was updated in
  the same commit.
  Verified: no live check recorded; existing configs load through defaults.

## 2. Tray surfaces on Windows and macOS

- `de64282d` **feat(agent): read the battery out in the tray tooltip, menu, and badge** — kept.
  Why: the tray thread owned HID I/O, so a readout there would duplicate the
  agent's state and reads.
  Fix: `BatterySummary` reads the shared `ObservableState`; tooltip, capped menu
  rows, an in-process digit badge updated through `NIM_MODIFY`, and the brand
  glyph as fallback; `tray_battery` off restores the old tray.
  Verified: Windows live checks recorded in `70345f20`.
- `3f9c2219` **feat(agent): read the battery out in the macOS menu bar** — kept.
  Why: the macOS status item had only the brand glyph.
  Fix: a watcher thread subscribes to `ObservableState` and coalesces one refresh
  on the main queue; title shows percentage or charging bolt; capped disabled
  device rows; `tray_battery` toggles live; the plan was updated in the same
  commit.
  Verified: **none on macOS** — the commit states it was neither compiled nor run
  on macOS hardware.
- `0eaae721` **feat(agent): draw the tray badge full-bleed with a style setting** — kept.
  Why: the badge did not fill the system icon slot; some users want the heavier
  rounded-square silhouette.
  Fix: full-diameter disc plus a serde-defaulted `tray_badge_style` rendered
  through the icon key so reload re-renders; radio group and locale keys; plan
  updated in the same commit.
  Verified: no separate check recorded; Windows badge rendering is covered by
  `70345f20`.
- `48691bd2` **fix(windows): make the tray thread per-monitor DPI aware** — kept.
  Why: the agent has no manifest, so the hidden tray window virtualized to 96
  dots per inch (DPI); the shell upscaled the 16 px badge into a 20 px slot and
  it read soft beside DPI-aware icons.
  Fix: per-monitor-v2 awareness on the tray thread; `WM_DPICHANGED` drives the
  existing refresh; the menu stays self-consistent because it pairs
  `GetCursorPos` with `TrackPopupMenu`.
  Verified: no measurement recorded.

## 3. Flow Indicator card

- `a71b8dcf` **feat(overlay): add the Flow Indicator battery card** — kept.
  Why: the tray only shows one reading; the plan calls for a glanceable list
  beside the desktop.
  Fix: a non-activating pop-up card with one row per device (kind glyph, name,
  charging/offline marker, tone-coloured percentage, bar), driven by
  `Observer::state`; config is mtime-polled; position lives in
  `flow-indicator.toml`; the card hides when disabled or empty; Normal strips
  the Windows TOPMOST bit and other platforms map the mode through the window
  kind.
  Verified: no live check recorded here; `c2cec378` verified the window
  behaviour.
- `cbc8f59c` **feat(desktop): add the Flow Indicator settings tab** — kept.
  Why: the new surfaces needed switches, the offline-row switch, the row limit,
  and the display mode.
  Fix: a tab after General, the 1–5 row limit, the Normal/Topmost radio group,
  locale keys in every catalog in the same order, and the 16 MB main-thread
  stack reserve the overlay links.
  Verified: no separate record.
- `c7bd3466` **fix(linux): keep the tray and Flow Indicator cfg-gated code lint-clean** — kept.
  Why: the badge renderer chain, `BatterySummary::badge`, and the Windows
  window-kind mapping are unreachable behind `cfg(windows)`, so Linux
  `-D warnings` saw dead code; the pop-up's `_mode` parameter is used on other
  platforms and tripped the underscore-prefix lint there.
  Fix: gate the orphaned items with their consumer and split `window_options`
  per platform.
  Verified: no saved log; the Linux build is the check.
- `3d45b42c` **fix(overlay): create the config directory when saving the card position** — kept.
  Why: a fresh install has no config directory until the agent writes its
  config; the overlay can run against a mock first, so the position file never
  appeared on WSLg.
  Fix: create the directory before saving the position.
  Verified: found and rechecked on WSLg; recorded in `4a891393`.
- `c2cec378` **fix(overlay): decorate, place, drag, and size the Flow Indicator card** — kept.
  Why: server-side decorations covered the frameless design, Wayland had no
  layer surface, GPUI's `start_window_move` sends `_NET_WM_MOVERESIZE` with
  button 0 which xfwm4 ignores, and the 171 px window clipped the last row.
  Fix: request client-side decorations; prefer a layer shell on Wayland; drag
  and restore through an overlay-owned X11 connection with `XQueryPointer` and
  BadWindow retirement; derive `card_size` from real line boxes and pin it with
  a `gpui::test`.
  Verified: X11/XFCE and nested labwc; plan §12 rounds one to three.

## 4. Linux tray

- `df8f287d` **feat(agent): host the tray as a StatusNotifierItem on Linux** — kept.
  Why: Linux had no tray; one D-Bus implementation can serve X11 and Wayland.
  Fix: `ksni` registers a StatusNotifierItem (SNI) whose multi-size `IconPixmap`
  bitmaps come from the shared badge renderer; the menu reuses the readout rows
  plus Show/Quit; a missing watcher is a soft failure; `TrayQuit`, the battery
  and badge-style settings, and overlay eviction now cover Linux; the quit hands
  off to the lifecycle instead of blocking inside the tray runtime.
  Verified: live on XFCE 4.20 (built-in SNI watcher): registration, tooltip,
  badge, Activate opening the GUI, dbusmenu rows, graceful quit; plan §12 fourth
  round.

## 5. Demo device source and mock tree

- `338f81c2` **feat(agent): serve the scripted demo devices from debug builds** — partly superseded by `112ba2a1`/`45e3db1f`.
  Why: developers and CI need the canonical demo inventory without HID I/O or
  the input hook, and the mock lived in a separate binary.
  Fix: a debug agent defaults to the demo (three Bolt-paired devices, a direct
  mouse, a standalone light); `OPENLOGI_MOCK_DEVICES=0` restores hardware; a
  release build always drives hardware; the mock moves into
  `openlogi_agent::mock` so the demo core composes production IPC, tray, and
  shutdown; `openlogi-agent-mock` remains for `--fixture` runs; plan updated in
  the same commit.
  Verified: no separate record.
- `d4d96278` **feat(agent): publish the demo light's readback state** — kept.
  Why: GUI surfaces and the tray need the light's power and brightness in the
  demo tree, not just in the real watcher.
  Fix: seed from recorded capabilities at saved defaults, move by `set_light`
  and `set_light_manual_power` writes, serve in `AgentSnapshot.lights`.
  Verified: no separate record.
- `37662f8e` **fix(agent): start the overlay in the demo tree** — kept.
  Why: the demo path never spawned an overlay, so the Flow Indicator could not
  appear in a debug session whatever the setting said.
  Fix: start the overlay supervisor at the production arming point and evict it
  on tray Quit; plan updated in the same commit.
  Verified: live — agent logs `helper started`, the card renders the demo
  devices, and toggling `flow_indicator` closes/opens it within the one-second
  poll; plan §12.
- `d211ee55` **fix(agent): apply tray settings on config reload in the demo tree** — kept.
  Why: the mock accepted reload RPCs as no-ops, so tray readout and badge-style
  changes needed an agent restart; the real agent applied them live.
  Fix: move the tray-facing reload into `openlogi-agent::reload::apply_tray_settings`,
  called by the real handler and installed as the mock's hook; the standalone
  mock binary leaves the hook empty.
  Verified: live — `wide` label read `MX Master 3S 80%`; switching the file to
  `rounded_square` cleared the label on the same agent; plan §12.
- `112ba2a1` **feat(core): resolve the device source at runtime** — kept.
  Why: the build tree, not the launch, chose the source; a release build could
  not run the demo and a debug build could not opt into hardware cleanly.
  Fix: `--mock` wins over `OPENLOGI_MOCK_DEVICES`; a WSL guest defaults to the
  demo because it exposes no HID devices; the launch decides in any build.
  Verified: no live check recorded; the dev workflows are documented in
  `13eccd1b`.
- `18947f39` **feat(ipc): add a status probe and request_shutdown (v43)** — kept.
  Why: a GUI switching the suite's device source must not kill the agent, and
  must read its status without waking a dormant agent.
  Fix: protocol v43 adds a status probe that does not declare a client and a
  `request_shutdown` that leaves through the lifecycle.
  Verified: no protocol test recorded; consumed by `622ab1e7`.
- `45e3db1f` **feat(agent): pick the device source from the launch, not the build tree** — kept.
  Why: the runtime source decision needed an owner on the agent side too.
  Fix: `--mock` (or `--fixture`, which implies it) serves the demo in any build;
  an unknown argument is a caller mistake; the IPC server gains the shutdown
  request; demo and real lifecycles evict the overlay when a client asks them to
  leave.
  Verified: no separate record.
- `622ab1e7` **feat(gui): align the running agent to the launch's device source** — kept.
  Why: a GUI launched for one source could meet an agent serving the other, and
  the spawn reflex would otherwise start a second source.
  Fix: read `--mock` (WSL defaults to the demo), probe without waking, ask the
  other source to leave through `request_shutdown`, then spawn the requested one;
  config, locks, and endpoint stay shared.
  Verified: no separate record.
- `2f7971ee` **feat(xtask): start the dev bundle's agent in demo mode by default** — kept.
  Why: the macOS dev bundle should match the GUI's default source.
  Fix: the runner starts the agent in demo mode and passes the same switch to the
  GUI; `OPENLOGI_DEV_REAL=1` restores hardware.
  Verified: no separate record.
- `b6bf867f` **fix(agent): name the demo devices by their product names** — kept.
  Why: the demo mouse carried a synthetic name, so its offline row did not look
  like the rest.
  Fix: the canonical demo's never-online mouse now uses a real product codename
  (`MX Anywhere 3S`).
  Verified: no separate check recorded; the name is recorded in plan §12.

## 6. Tray readout breadth

- `ee869dfc` **feat(agent): list standalone lights in the tray menu with power and brightness** — kept.
  Why: tray rows were battery rows only, so a standalone light never appeared.
  Fix: the shared readout renders lights after battery rows — on word with
  brightness normalized over the light's own range, off word, dash while
  unread, offline word when gone; all three platforms render the same rows;
  tooltip, badge, and menu-bar title stay battery-only; the module becomes
  `tray_readout`.
  Verified: no separate record.
- `1c0eb32d` **feat(core): grade battery percentages into three colour bands** — kept.
  Why: every surface needed one band decision, not its own threshold.
  Fix: 70%+ green, 40–69% amber, below 40% red; charging and charge-complete
  stay green; fault stays red; a level-only reading keeps its firmware tone;
  `BatteryStage` and `BatteryInfo::color` own it.
  Verified: live samples of SNI pixmaps (55% amber, 25% red, 45% charging green)
  and GUI/overlay screenshots (79% green, card 55% amber / 71% green); plan §12.
- `638aae6c` **feat(agent): colour the tray badge by the percentage band** — kept.
  Why: the badge used one accent colour regardless of level.
  Fix: badge disc follows `BatteryInfo::color`.
  Verified: same live samples as `1c0eb32d`; plan §12.
- `bc398c24` **feat(gui): colour percentage readouts by the shared band** — kept.
  Why: the settings app's numeric readouts did not match the tray/card.
  Fix: battery and brightness percentages use the shared band.
  Verified: GUI screenshots (79% green, card 55% amber / 71% green); plan §12.
- `448b1c28` **feat(agent): name tray and card rows from persisted product names** — kept.
  Why: firmware codenames leaked into user-facing rows; the settings app already
  persists a product name and a custom name.
  Fix: `Config::paired_display_name` resolves custom name, then persisted product
  name, then codename; tray rows and the card share it; the GUI reloads the agent
  after an identity or custom-name save.
  Verified: live D-Bus menu (`MX Master 3S 77%`, `MX Keys 100%`,
  `MX Vertical 55%`); plan §12.
- `181a7d02` **feat(agent): add the wide Linux tray face behind a config style** — kept; GNOME behaviour later extended by `ce2ffcc9`.
  Why: a circular badge wastes the horizontal room a panel gives a tray item.
  Fix: `tray_badge_style = "wide"` renders name, progress bar, and percentage as
  one SNI pixmap (142×22 at the slot, 284×44 at 2×); off by default; plan updated
  in the same commit.
  Verified: the face renders as designed, but XFCE 4.20 scales any tray pixmap
  into its icon slot, so it becomes a ~33×6 stripe and stays off by default;
  plan §12.
- `cc5fa9de` **fix(agent-core): name low-battery notices by the persisted product name** — kept.
  Why: the notice used the firmware codename while the tray and card used the
  product name.
  Fix: custom name, then persisted product name, then codename, in both the
  production orchestrator and the demo mock.
  Verified: no separate record.
- `fe92d9a5` **feat(agent): confine the wide tray face to desktops that can show it** — kept.
  Why: XFCE's systray scales the wide pixmap into its icon slot, making it
  unreadable.
  Fix: when the session is XFCE (environment or an `xfce4-panel` host process),
  fall back to the circle badge; plan updated in the same commit.
  Verified: XFCE live observation; plan §12.
- `ce2ffcc9` **feat(agent): show the wide tray readout as a GNOME label** — kept.
  Why: GNOME's AppIndicator extension shows no SNI tooltip and forces every
  pixmap into a square slot, so the wide face had no way to appear there.
  Fix: `tray_badge_style = "wide"` means "label" on GNOME — circle badge plus
  `MX Master 3S 77%` via `XAyatanaLabel`; vendor `ksni` 0.3.7 with the label
  properties, `XAyatanaNewLabel`, and the `PropertiesChanged` entry GNOME
  repaints from; `[patch.crates-io]` wires it in; plan and `CONFIGURATION.md`
  updated in the same commit.
  Verified: headless GNOME panel shows `MX Master 3S 80%` and follows
  80% → 79% → 77%; XFCE and KDE keep their previous faces; plan §12.

## 7. Overlay appearance, lifecycle, and windows

- `a3cdb782` **fix(overlay): follow the desktop appearance on Linux** — superseded by `a6542197`.
  Why: GPUI reads the XDG settings portal once at startup and never retries; a
  login-autostarted overlay can beat `xdg-desktop-portal` onto the bus, so the
  card kept the light palette for the whole session; the view never repainted on
  a later change.
  Fix: the overlay watches the portal (retrying the read, then following
  `SettingChanged`) and repaints the open card; macOS/Windows repaint through
  `observe_window_appearance`.
  Verified: GNOME Mutter ScreenCast captures — dark card in a dark session,
  light after a live switch, dark again; a login-launched overlay stayed light;
  plan §12.
- `14b60b04` **fix(agent): keep the tray on zbus async-io** — kept.
  Why: `ksni`'s default `tokio` feature enabled `zbus/tokio` for the joint agent
  and overlay build; the overlay's zbus then chose Tokio's executor on threads
  with no runtime, panicking GPUI's portal and the appearance watcher.
  Fix: use `ksni`'s `async-io` feature, the executor the workspace already uses;
  tray futures still run on the agent's runtime.
  Verified: panics gone; card follows light/dark live again; wide label, badge,
  and menu still work; plan §12.
- `7962022d` **fix(overlay): wait for the compositor and log the card's lifecycle** — kept.
  Why: at login the agent starts the helper before the Wayland socket exists;
  GPUI panics with `NoCompositor` (exit 101) and only the supervisor's 4 s retry
  keeps it alive; a retried process could end up without a card while its handle
  looked open.
  Fix: `platform::await_compositor` waits for the socket; the overlay logs its
  settings, first observation, and card open/close.
  Verified: plan §12; the lifecycle logs are the diagnostic.
- `3596989b` **fix(overlay): keep the card's window across hide and row changes** — kept.
  Why: Wayland gives clients no way to place a toplevel and Mutter ignores the
  requested origin, so closing the card on a settings toggle or row-count change
  recreated it and lost a dragged position.
  Fix: keep the window; disabled/empty shrinks it to a transparent 1×1; a
  row-count change resizes in place; only a display-mode change recreates it.
  Verified: GNOME logs `card hidden`/`card shown` with no new `card opened`; a
  3→2→3 row change resizes in place with the same top-left; plan §12. Across
  restarts, GNOME Wayland still cannot restore the position.
- `4a7ebed5` **fix(overlay): hide the card window natively on windows** — kept.
  Why: shrinking the card to 1×1 does not hide it on Windows — GPUI creates
  pop-ups as `WS_OVERLAPPED`, Windows clamps them to the frame minimum, and the
  Desktop Window Manager (DWM) keeps drawing the frame and its shadow around the
  1×1 client area.
  Fix: `ShowWindow(SW_HIDE)` / `SW_SHOWNOACTIVATE` through the live window handle;
  Wayland keeps the transparent 1×1.
  Verified: no saved record.
- `0f083d8b` **fix(overlay): strip the caption frame from the card's window** — kept.
  Why: a freshly opened Windows card kept the Win32 caption/frame GPUI leaves on
  the pop-up.
  Fix: clear `WS_CAPTION`/`WS_THICKFRAME` and apply `SWP_FRAMECHANGED`.
  Verified: no saved record.
- `f67c6b9d` **fix(overlay): name the overlay windows in the task list** — kept.
  Why: the overlay windows showed up unnamed in the Windows task list.
  Fix: set titles on the lock-key indicator, Actions Ring, backlight, and
  notice windows.
  Verified: no saved record.

## 8. Window placement memory

- `9670af79` **feat(gui): remember the main window's frame** — kept.
  Why: the settings window reopened at the compositor's placement; on Windows it
  drifted downhill across relaunches because of caption-placement bias.
  Fix: save the frame beside the config (`window-state.json`) once a second and
  on teardown; clamp it into the primary display on restore; correct the Windows
  caption bias; Wayland keeps the compositor's placement.
  Verified: no saved record.
- `3fe9a17f` **feat(hook): remember window placement in the gnome extension** — kept.
  Why: GNOME does not let a Wayland client place its own toplevel, so the main
  window and the Flow Indicator card could not restore their geometry.
  Fix: the extension moves both to their saved geometry after they open, saves
  what the user leaves, and keeps transient OSD surfaces out of the
  window-attention notifications.
  Verified: no saved record.

## 9. Pinned card and shadow

- `32120adc` **feat(overlay): pin the Flow Indicator card from its own button** — kept; the halo mechanism was replaced.
  Why: the card had no way to stay in place, and a pinned transparent HUD needs
  click-through and a shadow GPUI does not provide.
  Fix: header pin toggle; drag area removed; Topmost forced; transparent
  background; a hand-layered four-way halo on icons, text, and bars; the button
  occludes its own hitbox.
  Verified: none recorded for this first cut; later commits measured the
  click-through and shadow.
- `d950a687` **fix(overlay): shadow the pinned card's text and pass the mouse through** — kept; the shadow copies were later replaced.
  Why: the halo copies had no offset, so they sat behind the glyphs; the pinned
  card still swallowed clicks.
  Fix: shift each copy by its step while keeping the truncation inset; a native
  watch thread clears `WS_EX_TRANSPARENT` over the pin button and sets it
  elsewhere, writing only on change.
  Verified: none recorded here; `0c05eb7a` measured the click-through.
- `0c05eb7a` **fix(overlay): make the pinned card's halo visible and its window click-through** — kept; the halo colour was later replaced.
  Why: the white halo was invisible over a light desktop and ragged at 125% DPI;
  `WS_EX_TRANSPARENT` alone kept swallowing clicks (`WindowFromPoint` still
  answered the card).
  Fix: black halo in both themes on whole device pixels; add `WS_EX_LAYERED`
  beside the transparent bit and manage both as one switch; stop rewriting the
  topmost bit every observation.
  Verified: `WindowFromPoint` measurement before/after; commit.
- `ba5d285b` **fix(overlay): detach the pinned card's halo from the glyphs it shadows** — superseded by `0d199dd1`/`a29d855b`.
  Why: a halo copy hugging the glyph at full strength read as a bold weight with
  an outline.
  Fix: start two device pixels away; fade over three rings at 0.32/0.16/0.07;
  poll the click-through watch at 15 ms and trim the grace band to one pixel.
  Verified: none recorded.
- `0d199dd1` **fix(overlay): back the pinned card's content with a soft glow** — superseded by `a29d855b`.
  Why: the glyph-copy halo still read as bold; GPUI has no radial gradient and no
  text shadow.
  Fix: five concentric rounded backing layers per container, densest at the
  centre and reaching eight pixels past; the pin button gets the same; bars keep
  their box-shadow halo.
  Verified: none recorded.
- `a29d855b` **fix(overlay): blur the pinned card's shadow by stacking its shape** — superseded by `6adfdda6`.
  Why: upstream GPUI has no text shadow; the quality answer (signed distance
  field, SDF, glyph rendering) would mean forking the text renderer; four sparse
  directions left diagonal gaps that read as an outline.
  Fix: a twenty-offset disc kernel of translated copies composes a box blur.
  Verified: none recorded.
- `07231a4d` **fix(overlay): drop the pinned card's shadow a pixel below the glyphs** — reverted by `09e1c515`.
  Why: the blur centred on the text sat behind the glyph edges and read fuzzy.
  Fix: shift the kernel one device pixel down.
  Verified: none recorded.
- `09e1c515` **Revert "fix(overlay): drop the pinned card's shadow a pixel below the glyphs"** — revert of `07231a4d`.
  Why: the nudge did not fix the fuzzy edge.
  Fix: reverted the commit.
  Verified: not applicable.
- `a7c9fb3b` **fix(overlay): ring the pinned card's glyphs between edge and blur** — superseded by `1ff651ec`.
  Why: the centred blur softened the glyph's antialiased edge.
  Fix: an eight-copy ring one device pixel out at partial strength; the disc
  outside keeps the soft falloff.
  Verified: none recorded.
- `98e60ffc` **fix(overlay): border the pinned card's glyphs dark and glow light** — superseded by later border rework (`4a59aec3`/`cc88c08a`).
  Why: fixed border and glow colours did not match each row's own ink.
  Fix: border from the element's colour darkened; shadow lightened and spread to
  five device pixels; bars keep a black halo.
  Verified: none recorded.
- `941d0c06` **fix(overlay): thin the border to half a pixel and spread the glow to 18px** — reverted by `88136d0a`.
  Why: the border read too thick and the glow too short.
  Fix: half-pixel offsets; ring kernel with a dense 1–4 px core then eight
  directions every two pixels out to 18 px.
  Verified: none recorded.
- `88136d0a` **Revert "fix(overlay): thin the border to half a pixel and spread the glow to 18px"** — revert of `941d0c06`.
  Why: the half-pixel ring did not survive rasterization.
  Fix: reverted the commit.
  Verified: not applicable.
- `e12d7ad3` **feat(overlay): grow the pinned card's shadow into a far glow** — reverted by `794d4fc5`.
  Why: stamped copies cannot leave the five-pixel neighbourhood without showing
  glyph detail; a surface cannot paint past its own edge.
  Fix: grow the window by a glow margin; four rounded layers fade to the margin;
  shift the pin toggle back by half the growth; the click-through watch reads the
  button rectangle one margin inside.
  Verified: none recorded.
- `794d4fc5` **Revert "feat(overlay): grow the pinned card's shadow into a far glow"** — revert of `e12d7ad3`.
  Why: the grown-window approach did not produce the intended continuous field.
  Fix: reverted the commit.
  Verified: not applicable.
- `6adfdda6` **feat(overlay): draw the pinned card's far shadow with a blurred SVG** — kept.
  Why: stamps cannot produce a true far blur.
  Fix: `resvg` (already GPUI's SVG pipeline) renders `feGaussianBlur` of the
  names into an alpha mask GPUI tints; a content-hashed asset is served through
  the asset source; changing values stay out of the asset; the near field stays
  covered by stamps.
  Verified: none recorded separately; the two-scale tuning follows in
  `f49165c0`.
- `f49165c0` **fix(overlay): make the SVG far shadow visible and give it the bars** — kept.
  Why: a single five-pixel blur at 32% tint left almost nothing ten pixels out,
  and only the names contributed.
  Fix: blur at 2.5 and 8 pixels, at 55% each under a 40% tint; bar tracks join as
  capsules; percentage-driven fills stay out of the content hash.
  Verified: none recorded.
- `ba5f9448` **fix(overlay): follow the panel's theme and calm the dark glow** — theme part superseded by `a6542197`; glow calibration superseded by later shadow work.
  Why: the card followed only the OS appearance; the far glow always tinted
  black, so it was invisible on a dark desktop.
  Fix: read the panel's light/dark preference from the polled settings; tint the
  far glow white on dark and black on light; dim the dark theme's text one step
  and soften its stamped glow.
  Verified: none recorded.
- `1ff651ec` **fix(overlay): let the blurred SVG carry the pinned card's whole shadow** — kept.
  Why: three shadow mechanisms fought — the dark border and stamps pressed the
  glyphs into an outline, and the blurred bar capsules drew grey bands across the
  rows.
  Fix: remove the stamps and the border; the SVG blur carries a dense 2.5-pixel
  near pass and an eight-pixel far pass; the text is drawn crisp on top; the bar
  box shadow halves to a supporting role.
  Verified: none recorded.
- `8e73161a` **fix(overlay): point the pinned card's far glow downward** — reversed by `dbe9634c`.
  Why: the far glow was centred on each text; in the compact layout its upper
  half drifted into the row gap and read as a misplaced copy of the next line.
  Fix: translate the far blur five logical pixels down.
  Verified: none recorded.
- `21b56fd0` **fix(overlay): space the shadow rows by the row pitch, not the row height** — kept.
  Why: the generated SVG placed row *i* at *i* × row height, but the card lays
  rows one gap apart — 64 logical pixels between row tops, not 41 — so every
  row after the first had its shadow above its text, growing to 32 pixels by the
  third row.
  Fix: advance by height plus gap.
  Verified: the measurer confirmed the offset against the live card; commit.
- `dbe9634c` **fix(overlay): wear one shadow in both themes and weight it by ink** — kept; the tint later became one dark scheme in `a6542197`.
  Why: the downward nudge was compensation for the row-pitch bug; with the rows
  aligned it read as an offset; text and bars carry different amounts of ink.
  Fix: no nudge; one mid-grey tint; text shadows at 0.7/0.5 and bar shadows at
  0.2/0.12; remove the bars' second box-shadow mechanism.
  Verified: none recorded.
- `8332f19c` **feat(overlay): deepen the dark shadow and hairline the glyphs** — hairline mechanism superseded by `4a59aec3`.
  Why: the dark theme's shadow was too light against its light text, and the
  glyphs needed an edge.
  Fix: deeper grey in dark, lighter grey in light; a 0.1 px black edge from eight
  sub-pixel SVG copies between the blur and the text.
  Verified: none recorded.
- `4a59aec3` **fix(overlay): stroke the hairline border and darken the light shadow** — SVG stroke superseded by `47eaaedc`/`cc88c08a`.
  Why: eight sub-pixel copies rasterized each glyph at a different sub-pixel
  phase, so the edge came out patchy and doubled.
  Fix: a true SVG text stroke, 0.2 wide, a tenth of a pixel outside each glyph;
  both themes deepen (0.35 light / 0.25 dark at 0.6).
  Verified: none recorded.
- `47eaaedc` **fix(overlay): draw the names from one source so the border is the text's** — superseded by `b1f825b6`.
  Why: GPUI cannot stroke text, so a stroked copy could only sit behind the real
  glyphs and never matched them.
  Fix: the generated assets draw the blur, the 0.1 px stroke, and the fill from
  the same text elements; the name cell keeps its layout slot; pinned names
  truncate by character budget.
  Verified: none recorded.
- `b1f825b6` **fix(overlay): let the platform draw the pinned names again** — superseded by `cc88c08a`.
  Why: GPUI rasterizes SVG at twice the size and downsamples, losing the platform
  glyph rasterizer's hinting and gamma; pinned names looked rougher than the
  same text unpinned.
  Fix: names render through GPUI in both states; the assets keep only the blur
  and the 0.1 px stroke.
  Verified: none recorded.
- `cc88c08a` **fix(overlay): draw the hairline border with GPUI's own glyphs** — kept.
  Why: `resvg` resolved a different font face than GPUI, so the copied outlines
  widened as the string advanced and the edge drifted off the letters.
  Fix: the border comes from eight one-pixel copies drawn through GPUI's glyph
  atlas; the shadow asset keeps the blur only.
  Verified: none recorded.
- `3dd17d01` **feat(overlay): give the icons and readings the same border and glow** — kept; the reading capsule was superseded by `d90721fd`.
  Why: only the names had the ring and glow, so icons and readings looked
  inconsistent.
  Fix: the kind icon gets ringed SVG copies from the same glyph atlas; the
  percentage gets the name's eight-copy ring; the generated shadow embeds the
  vendored kind glyphs and a fixed capsule at the value's seat; charging and
  online bits join the content hash.
  Verified: none recorded.
- `d90721fd` **fix(overlay): shadow the reading with its own glyphs, not a capsule** — kept.
  Why: the capsule existed because the generated SVG cannot carry changing
  digits; every percentage would cache a new raster under a new path, and the
  sprite atlas never evicts SVG paths.
  Fix: sixteen low-alpha GPUI copies of the value text — glyph-shaped, aligned by
  construction, cache-free; the capsule leaves the shadow document.
  Verified: none recorded.
- `fba05f69` **fix(overlay): centre the kind glyph on the row's text line** — kept.
  Why: the icon glyph sat at the top-left of its 22 px lane, about two logical
  pixels above the text centre; the lane was centred, the glyph never was.
  Fix: centre the glyph in its lane; the ring copies ride its own box; the
  generated shadow's icon lands at the same centre.
  Verified: none recorded.
- `2aa211aa` **fix(overlay): give each theme its own decoration ink** — superseded by `a6542197`.
  Why: the black ring at 0.4 around the light theme's near-black glyphs read as a
  bolder, smeared weight; its near-black shadow fogged the white card.
  Fix: one per-theme owner picks both tints — dark keeps the black edge and
  deep-grey shadow, light takes a white edge and white glow; the blur asset
  carries no ink because GPUI tints its alpha mask.
  Verified: none recorded.
- `a6542197` **fix(overlay): keep one dark scheme for the floating card** — kept at HEAD.
  Why: the light palette put dark glyphs, dark ring copies, and a dark shadow on
  top of each other; on a light desktop the ring was quantized to device pixels
  and read as a ragged bright edge (measured 249 → 250…252 against the
  background), brightening the glyphs' antialiased edges; a dark ring and shadow
  smeared the letters.
  Fix: every overlay surface keeps the fixed dark scheme; the Linux settings
  portal watcher (`appearance.rs` and its zbus dependency), the view's preference
  and appearance observer, and the owner's revision polling are removed.
  Verified: 249 → 250…252 measurement against the background; commit.

## 10. Documentation checkpoints

- `8f27d0ed` **docs: add the battery display plan** — kept.
  Added `battery-display.md`: product design, shared reading semantics,
  architecture without IPC changes, settings, file map, milestones, and the
  verification matrix (513 lines).
- `70345f20` **docs: record the Windows live checks in the battery plan** — kept.
  Recorded the 125% DPI checks: tray tooltip reads the attached mouse, badge
  renders on dark and light taskbars, theme changes refresh the icon, Explorer
  restart re-adds it; named what remains per platform.
- `4a891393` **docs: record the WSLg Linux verification in the battery plan** — kept.
  Recorded that the non-desktop workspace compiles and passes Clippy/tests under
  WSLg, the overlay opens Wayland and X11 windows against the mock agent, and it
  persists its position; pixel verification needs a native Linux desktop.
- `4a30c268` **docs: record the Linux tray verification in the battery plan** — kept.
  Recorded the XFCE/labwc rounds: SNI registration, tooltip, badge, Activate,
  dbusmenu, graceful quit; card placement, drag, and sizing.
- `7eb5484c` **docs: record the GNOME session constraint for the Linux tray** — kept.
  Recorded that GNOME 50 on Ubuntu 26.04 is Wayland-only; the AppIndicator
  extension is installed and protocol-compatible; a visual GNOME check still
  needs a real Wayland session.
- `f6f81ead` **docs: record the GNOME appearance and tray-label findings** — kept.
  Recorded why the card stayed light in a login-autostarted session, and that
  GNOME renders `XAyatanaLabel` beside the tray icon while deliberately showing
  no SNI tooltip.
- `8b87be32` **docs: record the demo reload fix and GNOME's tray icon-size ceiling** — kept.
  Recorded the 16 px AppIndicator icon box and the `icon-size` setting that
  matches Ubuntu's text scaling, plus the card typography change and the demo
  tree's tray reload behaviour.
- `287064e9` **docs: record the overlay's login race and zbus executor findings** — kept.
  Recorded the `NoCompositor` crash at session start, the `ksni`/zbus Tokio
  feature leak into the overlay, and the diagnostics that now trace a boot's card
  lifecycle.
- `108f50da` **docs: record the card window's Wayland placement constraint** — kept.
  Recorded why a closed card loses its position on GNOME Wayland, the
  hide/resize behaviour that keeps it, and that cross-restart placement there
  needs a Shell extension.
- `13eccd1b` **docs: describe the runtime --mock device source** — kept.
  Documented that the launch, not the build tree, picks the source; the switch,
  the WSL default, the GUI's agent alignment, and the updated dev workflows.
- `ffff85ee` **docs(plans): record the overlay's single fixed colour scheme** — kept at HEAD.
  Replaced the palette table's light column and the appearance section, which
  described code removed by `a6542197`, with the one fixed dark scheme.

## 11. Style, chore, and mechanical

- `63e168b8` **style(overlay): size the card's device name like its value** — kept.
  The name was 13 px beside a 16 px percentage; both now read at 16 px so a row
  scans as one line. Recorded in plan §12.
- `154abc85` **style(gui): format the window-state clamp** — kept.
  Formatting only; no behaviour change.
- `05e041b6` **chore: ignore the local diagnostic scratch directory** — kept.
  Keeps local diagnostics out of Git.

## 12. Post-ledger work (2026-11-02)

These commits follow the ledger's original scope. Their entries were added with
`pending` hashes; the docs commit that follows them fills the hashes in.

- `e8f7de7c` **feat(overlay): refresh the Flow Indicator card** — kept.
  Why: the card carried three conflicting shadow mechanisms, the regular card
  could not follow the settings appearance after `a6542197`, the first pin could
  stall on GPUI's font-database scan, the Linux card reopened for a stacking
  change, and Windows clicks activated the overlay. Rows also had no way to say
  how many devices the limit hid, and newer device kinds had no icon.
  Fix: restore the Linux settings-portal watcher (`appearance.rs`, zbus) and keep
  only the pinned card dark; bake names and readings into path-only SVG shadows
  off the UI thread (`usvg`), with contact copies for the near field and
  per-reading assets; persist the pinned state; keep the card's window across
  row-count, DPI, and display-mode changes with a shadow margin; switch X11
  stacking live through `_NET_WM_STATE_ABOVE` and cut its input to the pin
  button with the Shape extension; make the Windows card non-activating; add
  headset/gamepad/camera icons and a `+N` hidden-row label.
  Verified: Windows `cargo check -p openlogi-agent -p openlogi-overlay
  --all-targets`; `cargo test -p openlogi-core -p openlogi-overlay
  -p openlogi-agent` (675 passed, 0 failed, 4 ignored); `cargo clippy
  -p openlogi-core -p openlogi-ui -p openlogi-overlay -p openlogi-agent
  --all-targets -- -D warnings`; `cargo fmt --all -- --check`; three offscreen
  Windows captures (`pinned-card`, `pinned-status`, `pinned-reading`) rendered
  and inspected; Linux `cargo check -p openlogi-overlay` in WSL (1.99.1)
  passed. Live desktop and hardware states remain unverified.
  Status: kept. This restores theme following for the regular card, reversing
  part of `a6542197`, while the pinned card stays dark.
- `62daaf5e` **feat(agent): raise the Flow Indicator with the GUI from the tray** — kept.
  Why: Show focused one GUI window and left the card behind; a second launch
  exits on the single-instance lock, so the card could not be brought up that
  way.
  Fix: enumerate the GUI's visible windows and the overlay's card (matched by
  `FLOW_INDICATOR_TITLE`), preserve their front-to-back order, raise normal
  windows through the topmost band, keep minimized and topmost windows as they
  are, and hand the foreground to the topmost GUI window — never to the card.
  Verified: five `plan_show` unit tests in the agent suite; the agent's 145
  tests (39 lib + 106 main) and Clippy `-D warnings` passed on the final tree.
  A live Windows tray raise was not recorded.
  Status: kept.

## 13. P3 overlay polish and pin sharing (2026-11-06)

These commits finish the P2/P3 Flow Indicator work from the working tree. The
docs commit that follows fills the hashes.

- `1cf7a29e` **feat(i18n): localize the Flow Indicator page and add pin labels** — kept.
  Why: every non-English catalog still carried the Flow Indicator settings page
  in English (including zh-CN), and the pin had no localized strings for its
  accessible label or the tray menu.
  Fix: translate the Flow Indicator section in the 23 non-English catalogs
  (Chinese reviewed; the rest best-effort for Crowdin) and add `pin_card`,
  `unpin_card`, and `more_devices` to every catalog in the same order.
  Verified: `cargo test -p openlogi-ui` passed (6 unit tests and the catalog
  parity test).
  Status: kept.

- `b6b460be` **feat(agent): share the Flow Indicator pin through the tray** — kept.
  Why: the pinned card had no keyboard or external path, and the tray and card
  needed one source for the pin; a second copy would drift.
  Fix: append `FlowPinnedObservation`, `observe_flow_pinned`, and
  `report_flow_pinned` to the IPC contract (v44); the agent owns a `FlowPin`
  seeded from `flow-indicator.toml` with a generation-stamped watch; Windows,
  Linux, and macOS trays render a checked item that toggles it; the CLI profile
  capture test agent implements the appended methods.
  Verified: Windows `cargo test -p openlogi-ipc -p openlogi-agent` (28 IPC,
  40 lib, 106 bin) and Linux (28 IPC, 41 lib) were recorded for the final tree;
  the live Windows tray click flipped the card and the placement file after the
  overlay wiring landed. macOS remains uncompiled.
  Status: kept.

- `15f208d4` **fix(hook): keep the pinned card click-through on GNOME** — kept.
  Why: Mutter kept routing clicks inside a focused card's old frame after the
  card's input region shrank to the pin, so the transparent HUD swallowed
  clicks until focus moved; the extension also did not know the card's pinned
  state.
  Fix: watch the placement file's `pinned` field, hand keyboard focus back to
  the last non-card window, and warp the pointer just outside the card and back
  so Mutter re-picks the input region; add a `RepickPointer` D-Bus diagnostic,
  bump the extension metadata, and document the live check.
  Verified: `node --test` in the extension directory passed 10 tests, including
  the new focus and pinned-state cases; the live GNOME pointer re-pick was not
  recorded.
  Status: kept.

- `156ee3e4` **feat(overlay): finish the Flow Indicator card's placement, input, and pin** — kept.
  Why: the shadow halo swallowed clicks, the saved origin changed meaning on
  differently scaled displays, Windows could move the card after a topology or
  DPI change, a dropped agent left frozen readings looking live, the panel
  language did not reach the card, the pin had no accessible name or hint, and
  long names kept their full baked shadow past the ellipsis.
  Fix: store the card origin in physical virtual-desktop pixels with its
  monitor and restore it across topology/DPI changes; apply the visible card or
  the pin as the native/input region (X11 input shape, Windows hit-test,
  skip-taskbar); mark readings stale after a five-second disconnect; poll
  settings and theme every 200 ms; carry the panel language into the shared
  catalog; add the pin's role, label, toggled state, and inline hint;
  pre-truncate the painted name before baking its shadow; and refine the
  contact/ambient shadow and per-track bar asset.
  Verified: Windows `cargo test -p openlogi-overlay` (109) and `cargo fmt
  --all -- --check`; Linux (WSL) overlay 112 tests and Clippy recorded before
  the split. Live Windows checks covered the halo hit-test, pin UIA toggle,
  tray/file round-trip, 125% capture, and the localized pin label. The stale
  disconnect path has unit coverage only. Linux X11 input region and card
  startup were checked; the GNOME pointer re-pick and macOS remain unverified.
  Status: kept.

## 14. Linux tray session boundary (2026-11-08)

- `0a1b4fc1` **fix(agent): retry the Linux tray when the session bus is not ready** — kept.
  Why: the tray is a graphical-session service, but the agent's systemd user
  unit can start outside the session's mount namespace. An xrdp/XFCE session
  keeps its D-Bus socket under a private `/tmp`, so ksni's connection failed
  permanently and the panel's systray host never saw an item.
  Fix: retry the SNI connection on a bounded 2 s / 5 s / 10 s backoff, log the
  `DBUS_SESSION_BUS_ADDRESS` and attempt number on each failure, and on the
  fourth failure give up with a message to start the agent inside the graphical
  session. The existing wait for a late StatusNotifier host is unchanged.
  Verified: Linux `cargo test -p openlogi-agent --bin openlogi-agent
  tray_linux::tests` (10 tests, including the retry window and unset-bus hint);
  Linux `cargo clippy -p openlogi-agent --all-targets -- -D warnings`; a runtime
  run with `DBUS_SESSION_BUS_ADDRESS=unix:path=/tmp/nonexistent` logged three
  retries and the final session-boundary message after four attempts.
  Status: kept.
