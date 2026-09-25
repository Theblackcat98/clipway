# Clipway — implementation plan

Target: Fedora GNOME on Wayland. Stack: Rust (gtk4-rs + Libadwaita) daemon,
tiny GJS Shell extension for capture, encrypted SQLite store.

The plan is ordered so every phase ends with something runnable. Nothing in
a later phase is load-bearing for an earlier one.

---

## Phase 0 — Validation spikes (2–4 days, before any architecture commits)

The whole project rests on five assumptions. Prove or kill each with a
throwaway spike before writing real code.

- **S0.1 — Capture works at all.** On Fedora's GNOME, confirm
  `St.Clipboard.get_default()` fires a usable owner-change signal for
  `St.ClipboardType.CLIPBOARD` and `get_text()` returns the new content.
  Check the GNOME 47/48 API surface while you're in there.
- **S0.2 — Image/file capture feasibility.** `St.Clipboard` only exposes
  text. Determine whether the extension can get images without blocking the
  compositor thread (`Gtk.Clipboard.wait_for_image()` is synchronous —
  likely a non-starter inside Shell). File copies usually arrive as
  `text/uri-list`, which *is* readable as text. Expected outcome: **text +
  file URIs in the MVP, images deferred** until the protocol backend
  (Phase 6) or an async read path is proven.
- **S0.3 — Source-app attribution.** At capture time, read
  `global.display.get_focus_window()` and resolve it with
  `Shell.WindowTracker.get_default().get_window_app()` to get a stable
  app-id. Needed for per-app exclusions.
- **S0.4 — Global hotkey.** `Main.wm.addKeybinding('clipway-popup', …)`
  with a bundled GSettings schema grabs `Super+V` reliably on Wayland.
- **S0.5 — Encrypted store in Rust.** Does `rusqlite`'s
  `bundled-sqlcipher` feature build cleanly on Fedora? If the SQLCipher
  packaging fight isn't worth it, the fallback is plain SQLite with the
  whole file encrypted via the `age` crate, key in the login keyring via
  `libsecret`. Decide here, not mid-build.
- **S0.6 — D-Bus plumbing.** A `zbus` service on the session bus as
  `org.clipway.Daemon`, callable from GJS via `Gio.DBusProxy`. Ten lines
  each side; just prove the round trip.

**Decision gate:** text-only MVP vs text+URI MVP, SQLCipher vs age.
Write the answers at the top of this file before proceeding.

## Phase 1 — Daemon skeleton (weeks 1–2)

Rust workspace:

```
clipway/
├── clipway-proto/    # D-Bus interface + shared types (serde)
├── clipway-daemon/   # GTK4/Libadwaita app, headless until popup
└── clipway-store/    # encrypted SQLite: schema, dedup, search, retention
```

- D-Bus API (`org.clipway.Daemon`):
  - `PushEntry { mime, data, source_app_id }` — called by the extension
  - `ShowPopup()`, `PasteEntry(id)`, `GetHistory(limit, query)`
  - Signal: `HistoryChanged`
- Store schema: `entries(id, hash UNIQUE, ts, mime, text, blob,
  source_app, pinned, trashed)`.
  - Dedup by SHA-256 of content; consecutive duplicates never stored.
  - Retention: cap by count (default 500) and age (default 30 days),
    enforced on insert. Pinned entries are exempt.
- Encryption: whole-store encryption decided in S0.5. Key from the login
  keyring, never a config file. First run generates and stores it.
- Runs as a `systemd --user` unit, no window until `ShowPopup`.
- Search: SQLite FTS5 over the text column — instant, no extra deps.

End state: `cargo run` starts a daemon you can push fake entries into over
D-Bus and query back. No UI yet.

## Phase 2 — Companion Shell extension (weeks 2–3)

`extension/` — GJS, deliberately kept around ~100 lines. It does exactly
three things and nothing else:

1. **Capture.** Connect to `St.Clipboard` owner changes, `get_text()` the
   new content, resolve the source app (S0.3), and call `PushEntry` —
   *unless* the app-id is on the exclusion list.
2. **Exclude.** The denylist (default: known password managers by app-id,
   user-editable later) is enforced **here**, so secrets never cross D-Bus
   into the daemon at all.
3. **Hotkey.** Grab `Super+V` (S0.4) and call `ShowPopup`.

All policy, storage, and UI live in the daemon. When GNOME's Shell API
churns each release, exactly one small file can break, and it's easy to fix.

End state: copy text anywhere → it lands in the encrypted store, attributed
to the right app; `Super+V` wakes the daemon.

## Phase 3 — Popup UI (weeks 3–4)

The part you'll actually see. Keyboard-first, Libadwaita, floating utility
window near the cursor:

- Search entry on top with instant FTS filtering; type-to-filter from the
  first keystroke.
- **Pinned** section, then **History**. Rows show a content preview, source
  app icon + name, and relative time.
- Keys: `↑↓` navigate, `Enter` select, `Del` remove, `Ctrl+P` pin,
  `Esc` dismiss. Mouse fully optional.
- Selecting an entry: the daemon puts it on the clipboard itself — any
  Wayland client can own the selection, no privilege needed.

End state: the core loop works — copy, `Super+V`, filter, `Enter`, paste.

## Phase 4 — Rich entries + real paste (weeks 4–5)

- **Files:** `text/uri-list` entries render as file rows (icon, name,
  count). Mostly falls out of Phase 2.
- **Images:** only if S0.2 found a non-blocking path. Otherwise stays on
  the Phase 6 list — be honest in the README about this.
- **Paste without formatting:** on select, offer/apply a strip to
  `text/plain` before the entry hits the clipboard.
- **Auto-paste:** daemon asks the extension over D-Bus; the extension
  synthesizes `Ctrl+V` through a Clutter virtual input device. This is the
  one feature that *needs* the extension's compositor trust — document why.

## Phase 5 — Harden + ship (week 6)

- Preferences window (GSettings): exclusion editor with running-app picker,
  retention sliders, encryption status, hotkey rebinding.
- Packaging: COPR RPM for the daemon + extension zip for
  extensions.gnome.org. No Flatpak — it needs session D-Bus and the
  extension anyway.
- CI: unit tests for store (dedup, FTS, retention, encryption round-trip).
  The extension can't run headless — cover it with a manual test matrix on
  Fedora N and N-1 instead of pretending.
- Docs: man page, README install section, threat model (what's encrypted,
  what's excluded, what never leaves the machine).

## Phase 6 — Future (when the platform catches up)

- **Protocol backend.** If/when Mutter implements `ext-data-control-v1`,
  add a capture backend behind the same trait and retire the extension's
  capture role (it may still own the hotkey until a Wayland hotkey portal
  exists).
- Revisit image capture via the protocol backend.

---

## Risks, stated plainly

| Risk | Mitigation |
|---|---|
| GNOME Shell API churn breaks the extension every 6 months | Extension surface is ~100 lines; breakage is localized and obvious |
| Image capture blocks the compositor or proves impossible | Text-first MVP; images are Phase 4/6, never load-bearing |
| SQLCipher Rust packaging pain | Age-fallback decided in Phase 0, not discovered in Phase 3 |
| `Super+V` conflicts with a distro default | GSettings-rebindable from day one |

## Suggested first three commits

1. `clipway-proto` + D-Bus round-trip spike (S0.6, kept)
2. `clipway-store`: schema + dedup + FTS + encryption, with tests
3. Extension skeleton: capture → D-Bus push → log (no daemon logic yet)

---

# As built (2026-09-24)

This section records what the Phase 0 spikes concluded and how the phases
actually landed. The plan above is unchanged; this is the record of reality.

## Phase 0 decision gate — answers

- **S0.1 / S0.2 — capture, and images.** `Meta.Selection` `owner-changed`
  is the trigger, not a clipboard signal: `global.get_display().get_selection()`
  emits it for the compositor's selection, and `St.Clipboard.get_mimetypes()`
  plus `get_content()` reads the payload for any MIME type. Images therefore
  do **not** need the deferred protocol backend: `image/png` is read
  asynchronously through `St.Clipboard`, verified against Pano's working
  GNOME 45–49 implementation. `Gtk.Clipboard.wait_for_image()` is indeed
  synchronous and is never used inside the shell process. MVP is therefore
  **text + images + file lists**, not text-only.
- **S0.3 — source-app attribution.** `global.display.focus_window.get_wm_class()`
  is read at capture time. `Shell.WindowTracker` is not needed; the WM class
  is what per-app exclusion matches on.
- **S0.4 — hotkey.** `Main.wm.addKeybinding('clipway-popup', …)` with a
  bundled schema works. One correction found by running the app: actions are
  activated over D-Bus through `org.gtk.Actions.Activate`, not
  `org.gtk.Application.ActivateAction`, which GLib does not export.
- **S0.5 — encryption.** `rusqlite` with `bundled-sqlcipher` builds cleanly
  on Fedora 42 (vendored amalgamation, system OpenSSL). The `age` fallback was
  not needed. The key is generated on first run and stored in the login
  keyring through the pure-Rust `keyring` crate.
- **S0.6 — D-Bus.** Two interfaces: `io.clipway.ClipboardManager1` (daemon:
  `AddEntry`, `GetRecent`, `PasteEntry`, `ClearHistory`, `HistoryChanged`) and
  `io.clipway.Extension1` (extension: `SetClipboard`). Session bus, `zbus` on
  the daemon, `Gio.DBus` in the extension.

## What shipped

- **Phases 1–3** exist as `daemon/` and `extension/`: encrypted store with
  dedup, caps, count-based eviction (pinned entries exempt), search over text
  and file paths, a keyboard-driven libadwaita popup with type-to-filter,
  pin, delete, and image thumbnails, a panel menu with recents, and the
  `Super+V` hotkey.
- **Phase 4** partially: file lists round-trip, images are captured, and
  primary-selection sync is implemented. Auto-paste is deliberately **not**
  implemented; the extension owns paste-back only.
- **Phase 5** partially: preferences (exclusions, depth, caps, incognito,
  primary-selection sync, clear-on-logout) and `make` targets for local
  install, checks, and extension zip. COPR/EGO packaging and the man page
  are not done.
- **Phase 6** untouched. Mutter still has no data-control protocol (verified
  against `src/wayland/meta-wayland.c` on `main`); the capture backend stays
  behind the extension.

## Deviations worth knowing

- Store is one crate (`daemon/`) rather than a `clipway-proto` +
  `clipway-daemon` + `clipway-store` workspace; the shared contract is plain
  D-Bus XML under `docs/dbus/`, generated from the same interface definition
  on both sides.
- Search is `LIKE` over the text column rather than FTS5: at the 500-entry
  default depth it is instant, and it keeps the SQLCipher amalgamation build
  free of the FTS5 compile flag.
- The store deduplicates on `(kind, mime, data)` rather than a SHA-256 hash
  column, which gives the same result without a second copy of the payload.
- The popup is a centered window, not a cursor-anchored one: Wayland forbids
  absolute positioning for ordinary clients, and GNOME has no layer-shell.
  The extension could position it in-shell later if that matters.
- The app opens its windows through GApplication actions so GNOME supplies an
  xdg-activation token and the popup can take keyboard focus.

## Verification state

`cargo build` and `cargo build --release` are warning-free; 17 unit tests pass
with and without the `gui` feature; `make check` (fmt, clippy, extension
syntax/metadata/schema lint) is green. An end-to-end run on a private session
bus confirmed capture of text and `image/png`, exclusion filtering
(`keepassxc` dropped), unreadable-as-SQLite storage, and history clearing.

Still open: live GNOME Wayland acceptance (popup focus with
`focus-new-windows=never`, paste-back, panel menu), a real screenshot for the
README — `clipway-daemon --screenshot FILE` renders the popup on any machine
with a display — and the Phase 5 packaging work.
