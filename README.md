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

- **Capture** lives in a tiny GNOME Shell extension whose only jobs are:
  watching `St.Clipboard`, forwarding new entries (text, image, files) over
  D-Bus with the focused app's identity, and grabbing the global hotkey.
  It's ~60 lines of JS and deliberately dumb — every bit of logic lives in
  the daemon, so GNOME API churn can only break one small, easily-fixed file.
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

Early design. The daemon and extension don't exist yet — this repo currently
holds the plan. The build order is:

1. `clipway-daemon` skeleton (GTK4/Libadwaita, D-Bus service, encrypted store)
2. Companion Shell extension (capture + hotkey + source-app reporting)
3. Popup UI with search, pinning, exclusions
4. Image/file entries, paste-without-formatting
5. data-control protocol backend (when Mutter supports it)

## Building

Nothing to build yet. When there's code, it'll be:

```sh
cargo build --release
```

with the extension installable from `extension/` via GNOME Extensions.

## Contributing

Issues and design discussion are welcome — especially from people running
GNOME on Wayland who've felt this gap. Rust and a little GJS are the working
languages.

## License

MIT. Clipboard history shouldn't be a moat.
