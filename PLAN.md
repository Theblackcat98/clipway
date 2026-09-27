# Clipway — implementation plan

Target: Fedora GNOME on Wayland. Stack: Rust (gtk4-rs + Libadwaita) daemon,
tiny GJS Shell extension for capture, encrypted SQLite store.

The plan is ordered so every phase ends with something runnable. Nothing in
a later phase is load-bearing for an earlier one.

> **Reading order.** The phases below are the original plan, kept for
> history. Where they conflict with reality, **"As built"** and
> **"Review and fixes (2026-09-26)"** at the end win. Notably: no default
> `Super+V` shortcut, the D-Bus names and methods changed, and settings live
> in the app's `io.clipway.Clipway` schema.

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
- **S0.3 — source-app attribution.** *(Corrected 2026-09-26.)* The WM class
  alone is not enough: on Wayland it is the app ID (KeePassXC reports
  `org.keepassxc.KeePassXC`, not `keepassxc`). The extension now sends
  `appId|wmClass|wmClassInstance`, with the app ID from
  `Shell.WindowTracker`, and exclusions match any of them.
- **S0.4 — hotkey.** *(Corrected 2026-09-26.)* The original call passed six
  arguments to `Main.wm.addKeybinding(name, settings, flags, modes,
  handler)`, named a key that did not exist, and the key had type `s`
  (Mutter requires `as`); `enable()` threw. No default shortcut is shipped
  (extensions.gnome.org rule; `<Super>v` is also GNOME's own
  notification-list shortcut). Actions are activated over D-Bus through
  `org.gtk.Actions.Activate`, not `org.gtk.Application.ActivateAction`,
  which GLib does not export.
- **S0.5 — encryption.** `rusqlite` with `bundled-sqlcipher` builds cleanly
  on Fedora 42 (vendored amalgamation, system OpenSSL). The `age` fallback was
  not needed. The key is generated on first run and stored in the login
  keyring through the pure-Rust `keyring` crate.
- **S0.6 — D-Bus.** Two interfaces: `io.clipway.ClipboardManager1` (daemon:
  `AddEntry`, `GetRecent`, `GetEntry`, `HistoryChanged`) and
  `io.clipway.Extension1` (extension: `SetClipboard`). Session bus, `zbus` on
  the daemon, `Gio.DBus` in the extension. *(2026-09-26: `PasteEntry` and
  `ClearHistory` were removed and both directions are now authenticated;
  see the review section.)*

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
- The app opens its windows through GApplication actions. *(Corrected
  2026-09-26: GNOME does not supply an activation token by itself. The
  extension now passes one in `platform_data`, and also focuses the app's
  window when it maps; in a headless GNOME Shell 46 test the window only got
  focus with the second mechanism.)*

## Verification state (2026-09-24, superseded below)

`cargo build` and `cargo build --release` are warning-free; 17 unit tests pass
with and without the `gui` feature; `make check` (fmt, clippy, extension
syntax/metadata/schema lint) is green. An end-to-end run on a private session
bus confirmed capture of text and `image/png`, exclusion filtering
(`keepassxc` dropped), unreadable-as-SQLite storage, and history clearing.

Still open: live GNOME Wayland acceptance (popup focus with
`focus-new-windows=never`, paste-back, panel menu), a real screenshot for the
README — `clipway-daemon --screenshot FILE` renders the popup on any machine
with a display — and the Phase 5 packaging work.

---

# Review and fixes (2026-09-26)

A review against the design doc found that the extension could not load,
that the D-Bus API let other programs read history and set the clipboard,
and a set of data bugs. Everything below was fixed on the `review-fixes`
branch. IDs match the review document (B = blocker, S = security/privacy,
C = correctness, P = performance, E = extensions.gnome.org/packaging).

## What was wrong, and what changed

| ID | Problem | Fix |
|---|---|---|
| B1 | `addKeybinding` called with 6 arguments, wrong key name, key type `s`; `enable()` threw after capture was already connected, so capture kept running with the extension "off" | Correct call on `popup-keybinding` (`as`, default `[]`); `enable()` undoes partial setup if anything throws |
| B2 | `St.Button` passed to `addToStatusArea` (throws); menu could never open | `PanelMenu.Button` subclass with its own menu |
| B3 | Schema missing from the extension zip, installed to `gschemas/`, ID `org.gnome.clipway` | App schema `io.clipway.Clipway`; the extension ships none and reads the app's |
| S1 | Any session process could call `SetClipboard` (background clipboard hijacking) and plant cut markers | Only the process owning the **focused** window may set the clipboard; only text/PNG/JPEG/URI lists accepted |
| S2 | Daemon API open to every session process; the extension sent every copy to whoever owned the daemon's name | Daemon answers only the root-owned `gnome-shell` binary (same UID); extension sends only to a daemon at an installed path, addressed by unique name; `PasteEntry`/`ClearHistory` removed, `GetEntry` added |
| S3 | Default exclusions (`keepassxc`, `org.freedesktop.secrets`) never matched on Wayland | Match app ID or window class, case-insensitive, `.desktop` ignored; defaults for KeePassXC, Bitwarden, 1Password, Secrets, Seahorse |
| S4 | Nautilus `cut` markers stored and restored verbatim | `x-special/gnome-copied-files` is never read or restored; file lists are `text/uri-list`; old rows converted |
| S5 | Missing keyring entry → new key generated silently → crash loop; env key in release builds | Key generated only when no database exists; keyring retry for 90 s; recovery dialog / `--reset-history` (old file kept); `RestartPreventExitStatus=3`; env key only in debug/test builds; 0700/0600 permissions |
| S6 | Unauthenticated `PrepareForShutdown` could wipe history; logout watcher never matched | Watchers removed. A marker in `$XDG_RUNTIME_DIR` detects a new session at start-up (also covers crashes and power loss) |
| S7 | Panel "Clear History" deleted pins with no confirmation | All clears go through one dialog: "Clear history" keeps pins, "Delete everything" is separate; `secure_delete` on |
| S8 | Whole payload read into GNOME Shell before the size cap | `Meta.Selection.transfer_async` with `cap + 1` bytes |
| C1 | Second-resolution timestamps: same-second copies listed oldest first | Milliseconds, `ORDER BY … ts DESC, id DESC` |
| C2 | Pins counted toward depth; enough pins deleted every new copy | Eviction counts unpinned rows only |
| C3 | `UTF8_STRING`/`STRING` restored verbatim (Wayland apps can't paste them); duplicates across types | All text normalised to `text/plain;charset=utf-8` (`STRING` decoded as Latin-1) |
| C4 | Own writes re-captured; stale reads; drag-and-drop treated as a copy | Self-write flag (Mutter emits `owner-changed` synchronously), per-selection generation counter, DnD and null owners ignored |
| C5 | Read timeouts not removed in `disable()`; speculative API probing | `Gio.Cancellable` cancelled in `disable()`; probing removed |
| C6 | Clipboard text parsed as Pango markup in the popup | Plain `GtkLabel`s |
| C7 | GTK clipboard fallback claimed success and could panic off the main thread | Removed; failures are shown as a toast |
| C8 | Enter pasted the first row, arrows did nothing from search, stale list | Enter restores the highlighted row, arrows/PgUp/PgDn move it, list refreshes on `HistoryChanged`, Ctrl+P pins |
| C9 | No activation token; popup did not get focus | Token in `platform_data` **and** the extension focuses the app's window when it maps |
| P1–P3 | Every refresh loaded every full payload and decoded images; blob-compare dedup; 300-row `GtkListBox` | `preview`/`thumb` columns (96 px PNG made at capture), SHA-256 `hash` column with a unique index, `GtkListView` |
| E1–E7 | Metadata, default shortcut, AI-code signals, lint that only checked syntax, Makefile/service ordering | See `metadata.json`, `Makefile`, `eslint.config.mjs`, `data/` |

Schema v2 migrates v1 databases on first start (text normalised, cut
markers converted, seconds → milliseconds, duplicates merged keeping pins).
The GSettings path moved from `/org/gnome/clipway/` to `/io/clipway/Clipway/`;
old settings are not carried over.

## What we learned (keep these in mind)

- **Mutter emits `owner-changed` synchronously inside `set_owner`**, so a
  flag around `set_content()` is enough to ignore your own writes.
- **Mutter keeps one copy of every clipboard change** (best text or image
  type, 4 MB / 200 MB caps) and re-owns the clipboard when the source app
  exits. Expect a `null` owner followed by a memory-source owner; dedup must
  absorb it.
- **`transfer_async` takes a byte limit** — use it instead of reading
  everything and checking afterwards.
- **On Wayland the WM class is the app ID.** Match exclusions on app IDs.
- **Headless GNOME Shell starts in the Overview**, and a new window does not
  get focus from an `org.gtk.Actions.Activate` call alone. Having the
  extension call `Main.activateWindow()` when the app's window is shown is
  what worked.
- **With only the `user` session mode, `disable()` runs on every screen
  lock.** Anything the extension holds in memory is gone after a lock — keep
  state in the app.
- **Something must actually run GNOME Shell.** Syntax checks passed on an
  extension that could not load. `make headless-test` does; run it (in a
  container) before every release, ideally on the GNOME version you target.
- **Threat model:** an unsandboxed program running as the same user can read
  the database key from the Secret Service. The D-Bus checks stop Clipway
  from *adding* capabilities; they are not a defence against arbitrary
  same-user code.

## How it was verified

- `cargo test` (32 tests, with and without the GUI), `cargo clippy -D
  warnings` (both feature sets), `cargo fmt --check`, ESLint, schema
  dry-run.
- The GUI was compiled against GTK 4.14 / libadwaita 1.5 (the Cargo
  features were lowered to `v4_14` / `v1_5`; nothing newer is used).
- End-to-end in a **headless GNOME Shell 46** (Ubuntu 24.04 container, with a
  shim for `Extension.getLogger()`, which only exists from GNOME 48). The
  harness is in `tests/headless/` (`make headless-test`, container/VM only);
  it drives the shell through `org.gnome.Shell.Eval` via a test-only helper
  extension and simulates copies with `Meta.SelectionSourceMemory`:
  extension loads and registers the shortcut; copies simulated through
  `Meta.Selection` are captured; X11/Wayland text deduplicates; images get
  thumbnails; cut markers, incognito copies and oversized copies are not
  recorded; `GetRecent`/`AddEntry`/`SetClipboard` from a non-Shell process are
  refused; the panel menu restores an entry without re-capturing it; the
  popup opens focused, Down + Enter restores the second row through
  `SetClipboard`, Ctrl+P pins; disable releases the bus name and stops
  capture; restarting the daemon in the same session keeps history; a daemon
  run from an untrusted path receives nothing.

## Still to verify on a real Fedora 44 (GNOME 50) and 45 (GNOME 51) session

- [ ] Extension loads on 50 and 51 with no `getLogger` shim; the version list
      in `metadata.json` (`50`, `51`) is only honest after this.
- [ ] Popup focus with the activation token alone and with the fallback, with
      and without `focus-new-windows=strict`.
- [ ] Copies from real apps: GNOME Text Editor, Nautilus (copy and cut),
      Firefox (text, image), a screenshot, an XWayland app, `wl-copy`.
- [ ] KeePassXC: `x-kde-passwordManagerHint` visible in
      `Meta.Selection.get_mimetypes()`, and its app ID excluded.
- [ ] Real login keyring: first start, locked keyring at autologin, reset
      keyring → recovery dialog → `--reset-history`.
- [ ] `/proc/<pid>/exe` readable for gnome-shell under Fedora 45's
      restricted ptrace setting (the daemon's caller check depends on it).
- [ ] Screen lock/unlock cycle: capture resumes, no leaked sources (Looking
      Glass → Extensions).
- [ ] Clear-on-logout marker behaves with a second concurrent session and
      with lingering enabled.
- [ ] `uuid` domain `clipway.dev` is one you control before uploading to
      extensions.gnome.org; otherwise switch to `clipway@<account>.github.io`.
