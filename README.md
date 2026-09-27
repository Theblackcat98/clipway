# Clipway

A Wayland-native clipboard history manager for GNOME. No X11 session, no
Electron.

## The problem

On X11, clipboard managers are easy: any client can watch the selection. On
Wayland that door is closed by design — a client only learns about the
clipboard when it has focus. GNOME's Mutter doesn't implement the
data-control protocol (`ext-data-control-v1` or the older wlroots one), so
there is no privileged, compositor-sanctioned way for an ordinary app to
observe clipboard history at all.

The result is a landscape of bad options:

| Tool | Why it's not the answer |
|---|---|
| Diodon | The GNOME-native classic. X11-only, and upstream moved to low-maintenance mode (2026-07-24). |
| Shell extensions (various) | Capture works, but they're fragile across GNOME releases and usually just a menu with no search, no encryption, no real app. |
| XWayland watchers | Depend on Xwayland running and on Mutter's X11 selection bridge. |
| Clipmer | The polished 2026 option — Electron, and not open source. |
| KDE's Klipper | Great, if you're on KDE. |

Clipway is the missing thing: a native GNOME clipboard manager that keeps
Wayland's rules instead of working around them.

## How it works

On GNOME, clipboard capture requires compositor cooperation, so Clipway
splits the job:

```
┌──────────────────────────┐      D-Bus       ┌──────────────────────────┐
│  GNOME Shell extension   │ ───────────────▶ │  clipway-daemon (GTK4)   │
│  (one JS file)           │  new entry +     │                          │
│                          │  source app      │  store · search · UI     │
│  watches Meta.Selection  │                  │  popup · settings        │
│  owns the shortcut       │ ◀─────────────── │                          │
└──────────────────────────┘  restore request └──────────────────────────┘
```

- **Capture** lives in a GNOME Shell extension. It watches the
  compositor's selection (`Meta.Selection` `owner-changed`), reads one
  preferred type through `Meta.Selection.transfer_async` with a size cap,
  and forwards it with the focused app's identity. It also restores entries,
  owns the popup shortcut and shows a small panel menu. It ships no settings
  schema of its own: it reads the app's.
- **Everything else** is a native GTK4/Libadwaita app (Rust, gtk4-rs):
  history, search, pinning, exclusions, encryption and the UI.
- If Mutter ever implements `ext-data-control-v1`, a protocol-based capture
  backend can replace the extension's capture role.

## Features

- **History** — text, images and file lists, with configurable depth and
  per-type size caps. Text from X11 and Wayland apps is stored the same way,
  so the same text is one entry.
- **Search** — case-insensitive substring search over text and file paths.
- **Pinning** — pinned entries are never evicted and survive "Clear
  history" unless you choose "Delete everything".
- **Privacy defaults** — content that password managers mark as secret
  (`x-kde-passwordManagerHint`) is never recorded; common password managers
  are excluded by app ID; incognito mode; unpinned history is cleared when
  the session ends.
- **Encrypted local store** — SQLCipher. The key lives in the login keyring.
- **Keyboard-driven popup** — type to filter, arrows to move, Enter to
  restore, Delete to remove, Ctrl+P to pin, Escape to close. No shortcut is
  set by default; pick one in Settings (Super+Shift+V is suggested).

Restoring puts the entry on the clipboard; you then paste with Ctrl+V as
usual. Automatic pasting is not implemented.

## Security & privacy

- Nothing leaves the machine. No network code, no telemetry.
- Excluded apps are filtered in the extension, before data leaves GNOME
  Shell. Exclusion goes by the focused app at copy time, so copies made from
  a terminal (`pass`, `wl-copy`) count as the terminal.
- The daemon only answers GNOME Shell (the caller's executable must be the
  root-owned `gnome-shell` binary), and the extension only sends clipboard
  data to a daemon started from an installed path. A process that grabs one
  of the bus names first gets nothing.
- The extension only lets the **focused** application set the clipboard
  through it — the same rule Wayland applies to every client — so a
  background process cannot use Clipway to replace what you copied.
- Nautilus "cut" markers are never restored (they would make a later paste
  move files).
- Limits, stated plainly: an unsandboxed program running as you can ask the
  login keyring for the database key, as it can for any secret stored there.
  Sandboxed (Flatpak) apps cannot reach Clipway's D-Bus names by default.

## Status

Working draft v0.1.0 (September 2026). See [PLAN.md](PLAN.md) for the
milestones, the 2026-09-26 review, and what is still unverified.

- Tested: 32 unit tests; ESLint; and an end-to-end run inside a headless
  GNOME Shell **46** (capture, dedup, images, incognito, size caps, D-Bus
  access checks, panel restore, popup focus and restore, pinning,
  disable/enable, name squatting).
- Not yet tested: a live GNOME **50/51** session on Fedora, and a real
  keyring (the headless test used a development key).

## Building

Requires Rust 1.92+, GTK 4.14+, libadwaita 1.5+, and OpenSSL headers
(`openssl-devel`, used by the SQLCipher build).

```sh
make install                                  # app, schema, desktop file, services, extension
gnome-extensions enable clipway@clipway.dev   # after logging out and back in
```

Then open Clipway's settings and choose a shortcut.

For development:

```sh
make run       # install the daemon and run it in the foreground
make nested    # a nested GNOME Shell for testing the extension
make check     # fmt, clippy, tests, ESLint, metadata and schema checks
make headless-test   # end-to-end in a headless GNOME Shell (container/VM only)
clipway-daemon --screenshot popup.png   # render the popup to a PNG (needs a display)
```

Extension changes need a GNOME Shell restart: log out and back in, or use
`make nested`.

If Clipway reports that it can't open your history (for example after the
login keyring was reset), `clipway-daemon --reset-history` moves the old
database aside and starts a new, empty one.

## Contributing

Issues and design discussion are welcome — especially from people running
GNOME on Wayland who've felt this gap. Rust and a little GJS are the working
languages.

## License

MIT. Clipboard history shouldn't be a moat.
