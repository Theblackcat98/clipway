# Clipway

A Wayland-native clipboard history manager for GNOME. No X11. No Electron.

## The problem

On X11, clipboard managers are easy: any client can watch the selection. On
Wayland that door is closed by design — a client only learns about the
clipboard when it has focus. GNOME's Mutter doesn't implement the
data-control protocol, so there is no privileged, compositor-sanctioned way
for an ordinary app to observe clipboard history at all.

The result is a landscape of bad options:

| Tool | Why it's not the answer |
|---|---|
| Diodon | The GNOME-native classic. X11-only, and upstream moved to low-maintenance mode (2026-07-24). |
| Shell extensions (various) | Capture works, but they're fragile across GNOME releases and usually just a menu with no search, no encryption, no real app. |
| XWayland hacks | Running your clipboard through XWayland defeats the point of being on Wayland. |
| Clipmer | The polished 2026 option — Electron, and not open source. |
| KDE's Klipper | Great, if you're on KDE. |

Clipway is the missing thing: a genuinely native GNOME clipboard manager
that works with Wayland's security model instead of around it.

## How it works

The architecture is honest about the one hard constraint: **on GNOME,
clipboard capture requires compositor cooperation.** Clipway splits the job:

```
┌─────────────────────┐      D-Bus       ┌──────────────────────────┐
│  Shell extension    │ ───────────────▶ │  clipway-daemon (GTK4)   │
│  (~60 lines of JS)  │  new entry +     │                          │
│                     │  source app      │  store · search · UI     │
│  hooks St.Clipboard │                  │  popup · exclusions      │
│  grabs the hotkey   │ ◀─────────────── │                          │
└─────────────────────┘  paste request   └──────────────────────────┘
```

- **Capture** lives in a small GNOME Shell extension whose only jobs are:
  watching `Meta.Selection` and `St.Clipboard`, forwarding new entries (text,
  images, file lists) over D-Bus with the focused app's identity, owning the
  global hotkey, and rendering the panel menu. It is deliberately mechanical —
  every decision about history, storage, and search lives in the daemon, so
  GNOME API churn can only break one small, easily-fixed file.
- **Everything else** is a native GTK4/Libadwaita daemon (Rust, gtk4-rs):
  history, search, pinning, exclusions, encryption, and the UI. No web view,
  no JS runtime in the app itself.
- The capture backend is a trait behind an interface. When Mutter eventually
  implements the data-control protocol, the extension backend gets replaced
  by a pure-protocol backend and the extension retires.

## Features

- **History that actually keeps up** — text, images, and file lists, with
  configurable depth and per-type size caps.
- **Full-text search** — instant filtering across everything you've copied.
- **Pinning** — keep snippets, URLs, and code blocks one keystroke away.
- **Per-app exclusions** — password managers and anything else you name never
  touch the store. Exclusion is enforced at capture time, so secrets never
  even cross D-Bus.
- **Encrypted local store** — SQLCipher-backed SQLite. Your clipboard history
  is one of the most sensitive files on your disk; it's treated that way.
- **Keyboard-driven popup** — one global hotkey (default `Super+V`), type to
  filter, Enter to select. Mouse optional.
- **Paste without formatting** — strips rich formats down to `text/plain`
  before the entry hits the clipboard.
- **Portal-aware** — respects sandboxed apps' clipboard semantics; doesn't
  fight Flatpak.
- **Zero X11** — no XWayland dependency, no legacy code paths. Wayland-only
  by construction.

## Security & privacy

- Excluded apps are filtered **in the extension**, before data leaves the
  compositor process.
- The store is encrypted at rest; the key lives in the login keyring, never
  in a config file.
- No network code. No telemetry. Nothing leaves the machine, ever.
- D-Bus interface is session-scoped and policy-locked to the daemon's UID.

## Status

Working draft v0.1.0 (September 2026):

- `daemon/` — `clipway-daemon` in Rust: GTK4/Libadwaita UI, D-Bus service,
  SQLCipher-encrypted store with a keyring-held key, search, pinning,
  eviction, and the settings window.
- `extension/` — GNOME Shell extension: clipboard capture, paste-back, the
  `Super+V` hotkey, and the panel menu with recents.
- Tooling — `make install`, `make run`, `make check`, `make extension-zip`.

Builds warning-free, 17/17 tests pass, and the D-Bus service is verified
end-to-end (including encrypted storage and exclusion filtering). Not yet
verified: a live GNOME Wayland session. [PLAN.md](PLAN.md) tracks the
milestones.

## Building

Requires Rust 1.92+, GTK 4.18+, libadwaita 1.6+, and OpenSSL headers
(`openssl-devel`, used by the vendored SQLCipher build).

```sh
make install                                  # daemon, schemas, extension, systemd unit
gnome-extensions enable clipway@clipway.dev   # after logging out and back in
```

`Super+V` opens the search popup. For development:

```sh
make run       # run the daemon from the working tree
make check     # fmt, clippy, tests, extension lint
cargo run -- --screenshot popup.png   # render the popup to a PNG (needs a display)
```

Extension changes need a GNOME Shell restart to take effect: log out and
back in on Wayland, or `Alt+F2`, `r`, `Enter` on X11.

## Screenshots

A screenshot of the popup will be added here after the live-session
verification pass. In the meantime, `clipway-daemon --screenshot FILE`
renders the real popup widget tree to a PNG on any machine with a display,
which is also handy for bug reports.

## Contributing

Issues and design discussion are welcome — especially from people running
GNOME on Wayland who've felt this gap. Rust and a little GJS are the working
languages.

## License

MIT. Clipboard history shouldn't be a moat.
