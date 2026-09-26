# Design & Build Document — A Clipboard History Manager for GNOME on Fedora

**Target:** Fedora Workstation 44 (GNOME 50) now, Fedora Workstation 45 (GNOME 51, final targeted for 20 Oct 2026) next. Wayland-only.
**Date:** 2026-09-25 (revision 2 — fact-checked)
**Status:** Pre-implementation design doc. Everything here is written to be acted on. Items still worth confirming on your own machine are marked **[verify]**.

---

## Revision notes (what changed from revision 1)

Every claim in revision 1 was checked against upstream docs, source code, or the reference projects. The biggest corrections:

| # | Revision 1 said | Reality | Impact |
|---|---|---|---|
| 1 | Watch `St.Clipboard`'s `'selection-owner-changed'` signal | **`St.Clipboard` has no signals.** Watch `global.display.get_selection()` → `Meta.Selection` `'owner-changed'` `(selection, type, source)` | Code would not have run at all |
| 2 | `mimetypes` is a semicolon-separated string; `get_content(type, mimetypes, cb)` | `get_mimetypes()` returns an array; `get_content()` / `set_content()` take **one** MIME string | Code bug |
| 3 | `Main.wm.addKeybinding(name, settings, flags, handler)` | Signature is `(name, settings, flags, modes, handler)` — `Shell.ActionMode` was missing | Code bug |
| 4 | Default shortcut `Super+Shift+V` is "a good default" | EGO rule: extensions **MUST NOT ship default keyboard shortcuts for interacting with clipboard data**. Ship the key empty | Would fail review |
| 5 | Do **not** synthesize Ctrl+V — impossible on Wayland | Inside gnome-shell you *can*: Clipboard Indicator's "paste on select" uses a Clutter virtual keyboard to send Shift+Insert (Ctrl+Shift+Insert in terminals) | Feature F4 is feasible |
| 6 | Supporting compositors: …Cage, GameScope, river, Wayfire, Weston, Louvre…; "Mutter and Muffin are the two holdouts" | wayland.app lists Cage, GameScope, Louvre, Muffin, **Mutter 51**, river, Wayfire and Weston as **not** supporting `ext-data-control-v1`. Supporters: COSMIC, Hyprland, Jay, KWin, Labwc, Mir, niri, phoc, Sway, Treeland | Core conclusion unchanged; list was wrong |
| 7 | XWayland copies may be invisible to the extension; X11 tools can't see native Wayland apps | Mutter bridges X11 selections into `Meta.Selection` (Clipboard Indicator explicitly requests `UTF8_STRING`/`STRING`). Out-of-process XWayland watchers (Ringboard's X11 watcher, Clipmer) rely on that same bridge | A 5th architecture option exists |
| 8 | Mutter's clipboard caching is "compositor courtesy" | It's deterministic: on every copy Mutter saves **one** best MIME type (text ≤ 4 MB, images ≤ 200 MB) and re-owns the clipboard with it when the source app goes away | Changes re-entrancy and dedup logic |
| 9 | `metadata.json` with `"session-modes": ["user"]` and `"version": 1` | EGO: `session-modes` **must be dropped** if only `user`; `version` is deprecated/set by EGO. Clipboard access **must** be declared in the description | Would draw review comments |
| 10 | Extension stays alive across screen lock | With `user` mode only, gnome-shell calls `disable()` on lock and `enable()` on unlock | In-memory ("ephemeral") history is lost at every lock; storage design must account for it |
| 11 | Re-entrancy guard with a 250 ms timeout | `owner-changed` is emitted **synchronously** inside `set_owner`, so a flag around the call is enough; the timer also leaked a main-loop source (EGO violation) | Simpler, correct code |
| 12 | `St.ImageContent` is GNOME 48+ | Available since 45; what changed in 48 is `Clutter.Image` removal and the new `Cogl.Context` first argument | Minor |
| 13 | Prefs: `Gtk.ShortcutLabel`, `Adw.MessageDialog`, `Gtk.Accelerator.parse()` | `Gtk.ShortcutLabel` deprecated in GTK 4.18 → `Adw.ShortcutLabel` (libadwaita 1.8+); `Adw.MessageDialog` → `Adw.AlertDialog`; functions are `Gtk.accelerator_parse()` / `Gtk.accelerator_valid()` | Deprecated/nonexistent API |
| 14 | `this.menu.actor.add_key_receiver()`, `Main.activate_global`, `Main.wm.removedKeybinding` | These don't exist | Imaginary API — also an EGO rejection trigger ("imaginary API usage") |
| 15 | `x-special/gnome-copied-files` can make a later paste **delete** files; "actively unset it" | A restored *cut* marker makes a later Nautilus paste **move** files (not delete). You cannot unset one target of another app's selection — you simply never restore that MIME type | Hazard overstated; mitigation clarified |
| 16 | Middle-click proposal is `gtk-primary-button-warps` | The key is `org.gnome.desktop.interface gtk-enable-primary-paste` (gsettings-desktop-schemas MR !119, Jan 2026) | Wrong setting name |
| 17 | `Super+V` is "a common tiling shortcut" | In stock GNOME, `Super+V` opens the notification list / calendar | Wrong reason, same conclusion |
| 18 | `wayland-info \| grep -i data` to check for data-control | That also matches `wl_data_device_manager`, which every compositor has → false positive. Grep for `data_control` | Diagnostic would mislead you |
| 19 | Store under `this.metadata.path` (skeleton) | That's the extension's install directory — wiped on update. Use `GLib.get_user_data_dir()` | Data loss bug |
| 20 | Skeleton re-registered `changed::show-history` inside `_registerKeys()` | Every change added another handler (never disconnected) | Leak/EGO violation |
| 21 | Clipboard Indicator and GCH both paginate; both default to Super+Shift+V | GCH paginates (`Ctrl+P/N`) and uses Super+Shift+V. Clipboard Indicator does **not** paginate (scroll view, history size default 15) and defaults to `Ctrl+F9` | Reference accuracy |
| 22 | "One GitHub issue asserts Mutter gained opt-in support in 47" | No evidence found. Mutter's source tree has no data-control implementation as of mid-2026, and wayland.app shows Mutter 51 unsupported | Removed |

New since revision 1 was written: **GNOME 51 was released on 16 Sep 2026** and ships in Fedora 45. It removes `Clutter.get_default_backend()` (breaks the common virtual-keyboard recipe), deprecates actor `key-press-event` in favour of `Clutter.KeyController`, throws if `disable()` is async, and adds a built-in QR widget. See §1 and §10. EGO also added an explicit **"extensions must not be AI-generated"** rule — relevant if you draft code with an assistant (see §11).

---

## 0. TL;DR — the facts that shape the project

**Mutter implements neither `ext-data-control-v1` nor `wlr-data-control-unstable-v1`.** There is no standard Wayland protocol that lets an ordinary out-of-process application watch the clipboard on GNOME.

wayland.app's compatibility table lists **COSMIC, Hyprland, Jay, KWin, Labwc, Mir, niri, phoc, Sway and Treeland** as supporting `ext-data-control-v1`, and **Mutter 51, Muffin, Weston, Wayfire, river, Cage, GameScope and Louvre** as not supporting it. Mutter's `src/wayland` tree contains no data-control code, and the old wlr-data-control request (mutter#524) was closed in 2019.

Consequences:

- `wl-paste --watch`, `cliphist`, `clipman`, and any data-control-based daemon **will not work on GNOME.** `wl-paste --watch` exits with *"Watch mode requires a compositor that supports the data-control protocol."*
- The **`org.freedesktop.portal.Clipboard`** portal does not help. It creates no session of its own; it can only be attached (via `RequestClipboard()`, before `Start()`) to a **RemoteDesktop** or **InputCapture** session. Technically that session then receives `SelectionOwnerChanged`, but it costs a remote-desktop consent dialog and a screen-sharing indicator. Wrong shape for a clipboard manager.
- **XWayland watching works, with caveats.** Mutter mirrors the clipboard into X11 selections, so an X11 client using XFixes can observe changes. Ringboard switches to its X11 watcher on GNOME and Clipmer runs as an XWayland client for this reason. Downsides: depends on Xwayland running, MIME fidelity is whatever the bridge provides, and reports are mixed on whether every native-Wayland copy is seen reliably. It's a fallback, not a foundation.
- **The only first-class route is code running inside gnome-shell** — a GNOME Shell extension — which can use Mutter's internal selection API directly (`global.display.get_selection()` → `Meta.Selection`, read/write via `St.Clipboard`).

So: **clipboard *capture* must be a shell extension.** Everything else — storage, search, thumbnails, and even the main UI — can live in a separate native app. Since your goal is a "Linux native app," the architecture that fits best is **Option D in §3: a GTK4/libadwaita app + a thin capture extension**, which is the route CopyQ (14.0+, ships a "CopyQ Clipboard Monitor" extension), Strata and GPaste have taken. The pure-extension route (Option A) is still the fastest path to a working prototype.

> **Verify before you commit.** Install `wayland-utils` and run
> `wayland-info | grep -iE 'data_control'`
> Empty output = no data-control (expected on GNOME). Don't grep for `data`: `wl_data_device_manager` exists everywhere and will give a false positive.

---

## 1. Environment baseline

| Item | Value | Notes |
|---|---|---|
| Distro (now) | Fedora Workstation 44, released 28 Apr 2026 | GNOME 50 |
| Distro (next) | Fedora Workstation 45, final targeted 20 Oct 2026 (slips to 27 Oct / 3 Nov if blocked) | GNOME 51; beta shipped 15 Sep 2026 |
| Earlier | Fedora 43 (Oct 2025) shipped GNOME 49 and dropped the GNOME X11 session | |
| GNOME | 50 "Tokyo" (18 Mar 2026); 51 "A Coruña" (16 Sep 2026) | Shell 50.x on F44 — check `rpm -q gnome-shell mutter` rather than assuming a point release |
| Session | Wayland only | GNOME 50 removed X11 session support upstream; the `Alt+F2` → `r` restart is gone (`RunDialog._restart()` removed) |
| Shell internals | ESM (`gi://…`, `resource:///org/gnome/shell/...`), `Extension` base class | GNOME 45+ |
| Tools | `gnome-extensions pack` (long-standing), `gnome-extensions upload` (49+), `gnome-extensions install --print-uuid` (50+), `gnome-shell-test-tool --extension <zip> <test.js>` (50+) | |
| Logging | `this.getLogger()` on `Extension` (48+) | Prefixes messages with the extension name |
| GLib | `GLib.timeout_add_once()` / `idle_add_once()` (new in the GNOME 50 cycle) | One-shot sources; you still must remove them in `disable()` if pending |

**GNOME 51 changes that matter here:**
- `Clutter.get_default_backend()` removed → use `global.stage.context.get_backend()` (needed for the virtual keyboard in §7.1).
- Connecting `key-press-event` directly on actors is deprecated → use `Clutter.KeyController` via `actor.add_action()`.
- `disable()` must not be `async` (now throws).
- `St` widgets' `vertical` property removed → `orientation: Clutter.Orientation.VERTICAL`.
- `PopupMenu.open()/close()` take `{animate, fadeOnly}` instead of an animation enum.
- New `ui/qrCode.js` `QrCode` widget (useful for a "show as QR" transform).

**Development loop:** use the development-kit nested shell from the GNOME 49 porting guide:

```bash
dbus-run-session gnome-shell --devkit --wayland
```

(If it complains about a missing viewer, install the package that provides the Mutter devkit viewer — **[verify]** the package name on Fedora with `dnf provides '*mutter-devkit*'`.) Otherwise log out and back in. Budget for it: you'll do it hundreds of times.

---

## 2. Why GNOME clipboard managers are structurally different

### 2.1 The ownership and observation model

On both X11 and Wayland, the clipboard is served by the **client that copied**; when that client exits, its data goes with it (on X11 that's why the `CLIPBOARD_MANAGER`/`SAVE_TARGETS` convention exists). The real difference is **observation**: on X11 any client can watch selection changes via XFixes; on Wayland a regular client only learns about the selection when it has keyboard focus, and cannot take ownership on someone else's behalf. `ext-data-control-v1` (the standardised successor of `wlr-data-control-unstable-v1`) grants a privileged client exactly those powers: `get_data_device(seat)`, `selection` / `primary_selection` events carrying an `ext_data_control_offer_v1`, `set_selection()` / `set_primary_selection()`, and `create_data_source()` to serve stored data back. Mutter doesn't implement it. Hence §0.

### 2.2 What Mutter already does for you (and why it matters)

Mutter's `meta-clipboard-manager.c` runs on every clipboard change:

- It picks the **single** best MIME type it supports — images (`image/png`, `image/svg+xml`, `image/webp`, `image/jpeg`, `image/gif`, `image/bmp`, `image/tiff`, up to 200 MB) or text (`text/plain;charset=utf-8`, `text/plain`, up to 4 MB) — and copies it into memory.
- When the source app goes away (selection owner unset), it **takes ownership itself** with a `Meta.SelectionSourceMemory` holding that one saved type.

Consequences for you:

1. "Paste after the app closed" works for plain text and images, but **rich text, HTML and file lists are lost** at that point. Capture eagerly, on the signal.
2. When an app exits you will see **two more `owner-changed` emissions**: one with `source === null`, then one with Mutter's memory source carrying content you already have. Your dedup must absorb this without creating a new entry.
3. Mutter is already transferring the clipboard once per copy; your extension transfers it again. Keep your own reads cheap (one MIME type, size-capped).

### 2.3 What the extension actually gets

```
global.display.get_selection()                  → Meta.Selection
  signal 'owner-changed' (selection, selectionType, source)
      selectionType: Meta.SelectionType.SELECTION_CLIPBOARD | SELECTION_PRIMARY | SELECTION_DND
      source: Meta.SelectionSource or null (null = owner went away)
  .get_mimetypes(selectionType)                 → string[]
  .transfer_async(type, mimetype, maxSize, outputStream, cancellable, cb)   (15 s internal timeout)

St.Clipboard.get_default()                      → St.Clipboard   (a thin wrapper over Meta.Selection; no signals)
  .get_mimetypes(St.ClipboardType)              → string[]
  .get_text(type, (clipboard, text) => …)
  .get_content(type, mimetype, (clipboard, bytes /* GLib.Bytes */) => …)
  .set_text(type, text)
  .set_content(type, mimetype, bytes)           // ONE MIME type per selection
```

`St.ClipboardType.CLIPBOARD` = Ctrl+C/V, `St.ClipboardType.PRIMARY` = middle-click selection.

Two limitations to design around:

- **One MIME type on re-paste.** `St.Clipboard.set_content()` (and `Meta.SelectionSourceMemory`) offer a single MIME type. If you restore an HTML entry as `text/html`, apps that only accept plain text get nothing. For v1, restore text as `text/plain;charset=utf-8` and offer "paste as HTML" as an explicit action. Offering several formats at once would need a custom `Meta.SelectionSource` subclass in JS — possible in principle, untested **[verify]**.
- **Reads are async and callback-based.** Wrap them in promises before doing anything else.

API reference: `gnome.pages.gitlab.gnome.org/gnome-shell/st/class.Clipboard.html` and `gnome.pages.gitlab.gnome.org/mutter/meta/class.Selection.html` (both currently document version 51).

### 2.4 Re-entrancy (your own writes) and stale reads

When you call `set_content()` to restore a history item, Mutter's `set_owner` emits `owner-changed` **synchronously, before `set_content()` returns**. So a flag set around the call is sufficient — no timer:

```js
// clipboard.js — capture layer
import Meta from 'gi://Meta';
import St from 'gi://St';

const CLIPBOARD = St.ClipboardType.CLIPBOARD;
// Most preferred first. UTF8_STRING/STRING appear on copies from XWayland apps.
const TEXT_TYPES = ['text/plain;charset=utf-8', 'text/plain', 'UTF8_STRING', 'STRING'];
const IMAGE_TYPES = ['image/png'];
const SECRET_HINT = 'x-kde-passwordManagerHint'; // set by KeePassXC & friends, value "secret"

function getContent(clipboard, mimetype) {
    return new Promise(resolve =>
        clipboard.get_content(CLIPBOARD, mimetype, (_clip, bytes) => resolve(bytes)));
}

export class ClipboardWatcher {
    #clipboard = St.Clipboard.get_default();
    #selection = global.display.get_selection();
    #ownerChangedId = 0;
    #generation = 0;
    #selfWrite = false;
    #onCopy;
    #logger;

    constructor(onCopy, logger) {
        this.#onCopy = onCopy;
        this.#logger = logger;
        this.#ownerChangedId = this.#selection.connect('owner-changed',
            (_sel, type, source) => this.#onOwnerChanged(type, source));
    }

    #onOwnerChanged(type, source) {
        if (type !== Meta.SelectionType.SELECTION_CLIPBOARD)
            return;
        const generation = ++this.#generation; // invalidates any read still in flight
        if (this.#selfWrite || source === null)
            return;
        this.#capture(generation).catch(e => this.#logger.error(e));
    }

    async #capture(generation) {
        const offered = this.#clipboard.get_mimetypes(CLIPBOARD);
        if (offered.includes(SECRET_HINT))
            return; // a password manager asked clipboard managers not to record this
        const mime = [...TEXT_TYPES, ...IMAGE_TYPES].find(t => offered.includes(t));
        if (!mime)
            return;
        const bytes = await getContent(this.#clipboard, mime);
        if (generation !== this.#generation || !bytes || bytes.get_size() === 0)
            return; // superseded by a newer copy, or empty
        this.#onCopy(mime, bytes);
    }

    setContent(mime, bytes) {
        this.#selfWrite = true;
        try {
            this.#clipboard.set_content(CLIPBOARD, mime, bytes);
        } finally {
            this.#selfWrite = false;
        }
    }

    destroy() {
        this.#selection.disconnect(this.#ownerChangedId);
        this.#generation++; // pending reads will see a stale generation and drop out
        this.#onCopy = null;
    }
}
```

Notes:

- The generation counter also handles "a second copy lands while the first `get_content()` is in flight."
- Even with the guard, **dedup by content hash** (§9) is your real safety net: Mutter's takeover after an app exits (§2.2) and other extensions writing the clipboard will all look like copies.
- `St.Entry` text copied inside the shell (search field, notifications) and Mutter's takeover both show up as `Meta.SelectionSourceMemory` owners (`source instanceof Meta.SelectionSourceMemory`). You can use that as a hint, but don't ignore them outright — a copy from the shell's own UI is a legitimate copy.
- **[verify] in the M0 spike:** that `x-kde-passwordManagerHint` survives into `get_mimetypes()` on GNOME. KeePassXC offers it without a `type/` prefix; GTK-based *clients* are known to strip it, but Mutter is not GTK, so it should be preserved. Test with KeePassXC.

### 2.5 MIME type negotiation

Always inspect what's offered. Real-world advertisements:

| MIME type | Meaning | Handling |
|---|---|---|
| `text/plain;charset=utf-8` | Normal text | Store as text |
| `text/plain` | Unspecified charset | Decode as UTF-8 with `TextDecoder('utf-8', {fatal: false})`, strip NULs |
| `UTF8_STRING`, `STRING`, `TEXT` | Text from XWayland (X11) apps | Treat as text |
| `text/html` | Rich text (browsers, LibreOffice) | Store the plain text; optionally keep HTML for an explicit "paste as HTML" |
| `application/x-gtk-text-buffer-rich-text` | GTK internal serialised buffer | Skip |
| `image/png` | Screenshots, image editors | Store bytes, thumbnail lazily |
| `image/jpeg`, `image/webp`, … | Rarer | Whitelist or ignore in v1 |
| `text/uri-list` | File copies from Nautilus (also offered as `text/plain` paths) | Don't restore as a file list in v1 |
| `x-special/gnome-copied-files` | Nautilus copy/**cut** marker (`copy\n` or `cut\n` + URIs) | **Never restore it.** A restored `cut` marker makes the next Nautilus paste *move* files |
| `x-kde-passwordManagerHint` | Password-manager "don't record me" hint | Skip the entry (default) |
| `application/x-kde-cutselection` | KDE cut marker | Ignore |

A pragmatic v1 rule: capture text (the four text types) and `image/png`; skip anything carrying the secret hint; ignore the rest.

---

## 3. Architecture options

### Option A — Pure extension (fastest prototype)

Everything in `extension.js` + local files.

- **Pros:** No IPC, no daemon lifecycle, installs like any extension, one package on EGO. Matches Clipboard Indicator and Gnome Clipboard History.
- **Cons:** Runs on the compositor's main thread. JS search over thousands of entries and image decoding compete with frame rendering. JS itself is single-threaded, but note that **Gio's `*_async` file I/O and GdkPixbuf's async scaled loading run on worker threads**, so I/O and decoding needn't block. Encryption at rest is impractical (no crypto primitives in GJS). And with `user` session mode, `disable()` runs on **every screen lock**, so anything held only in memory is gone after a lock.

### Option B — Extension (UI + capture) + local daemon

The extension captures and renders; a daemon (e.g. Rust + zbus + SQLite) owns storage, search index and thumbnails. This is what **Strata** does: the extension checks the MIME allowlist, reads bytes and fires `SubmitItemAsync(mime, bytes)` over the session bus without awaiting, behind a 50 ms debounce.

- **Pros:** Heavy work off the compositor thread; storage testable in isolation; ephemeral history can survive screen locks (the daemon keeps running); encryption at rest becomes easy (SQLCipher + key in the login keyring via libsecret).
- **Cons:** Two packages. EGO forbids binaries inside the extension zip, so the daemon ships separately (RPM/COPR/Flatpak) and users need both halves. The daemon still can't watch the clipboard — only the extension can.

### Option C — Standalone app watching via data-control

Not possible on GNOME (§0).

### Option D — Native GTK4/libadwaita app + thin capture extension (**recommended for your stated goal**)

A normal app (history window, search, preferences, tray-less) plus a minimal extension that only (1) captures on `owner-changed`, (2) restores an entry when the app asks, (3) optionally sends the paste keystroke, and (4) exposes all this over D-Bus. CopyQ 14.0+ ships exactly such a "Clipboard Monitor" extension; Strata and GPaste are variations of it.

- **Pros:** A real native app in the language you like; the extension stays small, which makes EGO review and per-release porting cheap; shares Option B's lock-survival and encryption benefits.
- **Cons:** Opening the app window takes focus from the target app, so "paste into the previous window" (§7.1) needs care. **If you ship the app as a Flatpak, the extension still has to be installed separately** (CopyQ notes its extension can't be registered from Flatpak/AppImage). Define a versioned D-Bus interface and handle "extension present but app not running" and vice versa.

### Option E — XWayland watcher (no extension)

Run an X11 client under Xwayland that watches `CLIPBOARD` via XFixes (Ringboard's fallback; Clipmer polls under XWayland).

- **Pros:** No extension to port every six months.
- **Cons:** Depends on Mutter's X11 selection bridge and on Xwayland running; MIME fidelity limited; reports conflict on reliability for native-Wayland copies (CopyQ's maintainer has said it may only see copies from other XWayland apps). Paste-back can't synthesize keys. Useful as a fallback, not a foundation.

### Recommendation

- **Prototype (M0–M1):** Option A — fastest way to learn the capture behaviour and ship something you use daily.
- **Product:** Option D (or B if you prefer the UI inside the shell). Design the storage layer as a separate module from day one so the move is mechanical, and keep the extension's surface area small.

---

## 4. Feature specification

### 4.1 Core

| # | Feature | Detail | Difficulty |
|---|---|---|---|
| F1 | Capture | On `owner-changed`, read MIME list, fetch one preferred type, store | Easy |
| F2 | Dedup / resurfacing | Same content (by hash) → move existing entry up instead of duplicating | Medium |
| F3 | History panel | `PanelMenu.Button`, Quick Settings menu, or a modal overlay | Medium |
| F4 | Paste on select | Set clipboard, then synthesize Shift+Insert via a virtual keyboard (§7.1) | **Hard** (timing, terminals) |
| F5 | Search | Incremental, case-insensitive; regex optional | Medium |
| F6 | Pin / favourite | Pinned entries survive clear, separate section | Easy |
| F7 | Delete entry | Individual, with undo | Easy |
| F8 | Clear all | With confirmation, honouring pins | Easy |
| F9 | Private mode | A switch that stops recording | Easy |
| F10 | Excluded apps | Heuristic: app of the **focused window at copy time** (`Shell.WindowTracker.get_default().get_window_app(global.display.focus_window)?.get_id()`). The copying app isn't always focused (CLI tools, background apps) — document the limitation | Medium |
| F11 | Keyboard nav | Arrows/Home/End/Delete/Enter + type-to-search | Medium |
| F12 | Paste without replacing clipboard | Set entry → send paste → restore previous clipboard. Restoring too early races the target app's async read; start ~50 ms and tune | Medium |
| F13 | Image support | Capture `image/png`, thumbnails off the open path | Medium |
| F14 | Retention | Max entries, per-entry byte cap, total cap, optional expiry | Easy |
| F15 | Clear on lock / on boot | See §5.3 — interacts with `disable()`-on-lock | Easy–Medium |
| F16 | Honour password-manager hint | Skip entries offering `x-kde-passwordManagerHint` (default on) | Easy |

### 4.2 Worth considering beyond the obvious

- **Numbered quick-select** — `Ctrl+1..9` picks the Nth entry while the panel is open (GCH does this).
- **Cycle through history** — Clipboard Indicator binds prev/next entry shortcuts. `Meta.KeyBindingFlags.TRIGGER_RELEASE` (listed in Mutter 51 docs) could enable a hold-modifier-and-tap switcher **[verify]**.
- **Edit entry in place**, **preview pane**, **relative timestamps**, **"paste as plain text"**.
- **Transform actions** — URL-decode, trim, base64, case, JSON pretty-print; **QR code** via GNOME 51's `ui/qrCode.js` (51+ only; for 50 you'd need your own renderer).
- **Tags** (Clipboard Indicator has them).
- **Export/backup** — treat as a privacy feature too.

### 4.3 Skip for v1

Multi-format restore (§2.3); file-list support; KDE cut-selection interop; multi-device sync (security liability, and EGO forbids sharing clipboard data with third parties without explicit user action).

---

## 5. Storage design

### 5.1 Options

| Approach | Verdict |
|---|---|
| **JSON index rewritten on change** | Fine below a few hundred entries if writes are async and debounced |
| **Append-only op log + compaction** | Gnome Clipboard History's design: one amortized disk write per change, so every change can be written immediately. You write the op decoder and compaction |
| **SQLite** | Not practical inside gnome-shell (no binding installed by default). Natural in a daemon/app (Options B/D) — Strata uses SQLite, Clipway uses SQLCipher |
| **Content files + small index** | Keeps large images out of the index. Good default for A |

### 5.2 Recommended v1 (Option A): content files + JSON index

```
$XDG_DATA_HOME/<extension-uuid>/        (GLib.get_user_data_dir(), mode 0700)
├── history.json          # [{id, mime, hash, created, bytes, preview, pinned}]
├── content/              # one file per entry, mode 0600
└── thumbs/               # disposable
```

- **Never** store under `this.metadata.path` or `this.path` — that's the extension install dir and is replaced on update.
- Atomic, private writes: `Gio.File.replace_contents_bytes_async(bytes, null, false, Gio.FileCreateFlags.PRIVATE | Gio.FileCreateFlags.REPLACE_DESTINATION, null, cb)` writes to a temp file and renames, and `PRIVATE` gives user-only permissions. Create directories with `GLib.mkdir_with_parents(path, 0o700)`.
- Write policy: with a rewrite-the-index design, **debounce** (~500 ms) and do a final write in `disable()`. With an append-only log, write **immediately** (GCH's rule — "any history change is immediately written" — is affordable only because appends are cheap).
- Location: `GLib.get_user_data_dir()`, not `~/.cache` (pins aren't disposable, and cache cleaners may wipe it).

```js
// store.js — sketch of the write path
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';

Gio._promisify(Gio.File.prototype, 'replace_contents_bytes_async', 'replace_contents_finish');

const encoder = new TextEncoder();

export async function writeIndex(dir, entries) {
    GLib.mkdir_with_parents(dir, 0o700);
    const file = Gio.File.new_for_path(GLib.build_filenamev([dir, 'history.json']));
    const bytes = new GLib.Bytes(encoder.encode(JSON.stringify(entries)));
    await file.replace_contents_bytes_async(bytes, null, false,
        Gio.FileCreateFlags.PRIVATE | Gio.FileCreateFlags.REPLACE_DESTINATION, null);
}
```

### 5.3 Privacy (and the screen-lock trap)

This tool records everything the user copies, including passwords. Say so in the README and in the EGO description (EGO **requires** declaring clipboard access in the description).

- **Private mode**, **excluded apps**, and **honour `x-kde-passwordManagerHint`** by default.
- **"Only persist favourites"** (ephemeral history, durable pins). **Caveat for Option A:** because gnome-shell calls `disable()` on every screen lock and EGO requires you to free dynamic state in `disable()`, ephemeral history is wiped at every lock — probably not what users expect. Options: persist everything (encrypted if possible), accept wipe-on-lock and label the mode that way, or move the ephemeral store into a daemon/app (B/D). Declaring `unlock-dialog` just to keep state needs a justification reviewers may not accept.
- **Clear on lock** is nearly free in Option A: do it in `disable()` when the session is locking (e.g. check `Main.sessionMode.isLocked` **[verify]**).
- **Clear on boot**: clear at first `enable()` after boot (compare against boot ID from `/proc/sys/kernel/random/boot_id`).
- **Encryption at rest** is a real differentiator but belongs in B/D (key from the login keyring via libsecret; SQLCipher or libsodium in the daemon). GJS has no suitable crypto primitives.
- Files `0600`, directories `0700`. No network code, no telemetry (EGO forbids telemetry).

---

## 6. Keybinding API

### 6.1 Registration

```js
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

Main.wm.addKeybinding(
    'show-history',                 // key name in your GSettings schema
    this._settings,                 // Gio.Settings holding it
    Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
    Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW | Shell.ActionMode.POPUP,
    () => this._toggleHistory());

// in disable()
Main.wm.removeKeybinding('show-history');
```

- `Shell.ActionMode` controls where the binding is active; include `POPUP` if the same shortcut should also close your open menu. Clipboard Indicator uses `Shell.ActionMode.ALL`.
- `IGNORE_AUTOREPEAT` stops a held shortcut from firing repeatedly. Use it for "open panel".
- The return value is a `Meta.KeyBindingAction`; `NONE` means registration failed. Accelerator **conflicts** with other bindings are not reliably reported that way — detect them in prefs (§6.5).
- Whether Mutter picks up changes to the settings key live is not documented; Clipboard Indicator simply unbinds and rebinds whenever its settings change. Do the same (it's cheap), and connect the `changed::` handler **once**, in `enable()`.

### 6.2 Schema

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!-- schemas/org.gnome.shell.extensions.clip-history.gschema.xml -->
<schemalist>
  <schema id="org.gnome.shell.extensions.clip-history"
          path="/org/gnome/shell/extensions/clip-history/">
    <key name="show-history" type="as">
      <default>[]</default>
      <summary>Show clipboard history</summary>
      <description>Shortcut that opens the history. Empty by default; set it in Preferences.</description>
    </key>
    <key name="paste-on-select" type="b">
      <default>false</default>
      <summary>Paste into the focused app after choosing an entry</summary>
    </key>
  </schema>
</schemalist>
```

- Type `as` because a key can hold several accelerators.
- When you do write accelerator defaults, `<` and `>` must be escaped in XML — use `<![CDATA[['<Super><Shift>v']]]>` or `&lt;Super&gt;`.
- EGO: schema ID must start with `org.gnome.shell.extensions`, path with `/org/gnome/shell/extensions`, the XML must be in the zip, and the filename must be `<schema-id>.gschema.xml`.

### 6.3 Choosing (not shipping) a default

EGO's rule — *"An extension MUST NOT ship with default keyboard shortcuts for interacting with clipboard data"* — reads as covering "open clipboard history," so **ship the key empty** and offer a one-click suggestion in prefs. When picking what to suggest:

- `Super+V` — GNOME's own shortcut for the notification list/calendar. Avoid.
- `Ctrl+Shift+V` — paste in terminals, paste-as-plain-text in browsers. Avoid.
- `Super+Shift+V` — what GCH and Strata use; good suggestion.
- `Ctrl+F9` — Clipboard Indicator's toggle-menu default (F8–F12 for its other actions).

### 6.4 Autorepeat

Covered above: `IGNORE_AUTOREPEAT` for open/close. A deliberate "cycle" binding is a separate key with its own flags.

### 6.5 Shortcut editor in prefs

`prefs.js` runs in a separate GTK4/libadwaita process (never import `Clutter`/`Meta`/`St`/`Shell` there, and never `Gtk`/`Gdk`/`Adw` in `extension.js`).

1. An `Adw.ActionRow` showing the current accelerator with **`Adw.ShortcutLabel`** (libadwaita 1.8+, i.e. GNOME 49+). `Gtk.ShortcutLabel` is deprecated since GTK 4.18.
2. On activate, open an `Adw.Dialog` (or `Adw.AlertDialog`; `Adw.MessageDialog` is deprecated) with a `Gtk.EventControllerKey`.
3. On key press: `Escape` cancels, `BackSpace` clears; otherwise build the string with `Gtk.accelerator_name_with_keycode(...)`, validate with `Gtk.accelerator_valid(keyval, mods)`, write to GSettings.
4. Conflict check: compare against `org.gnome.desktop.wm.keybindings`, `org.gnome.shell.keybindings`, `org.gnome.mutter.keybindings`, `org.gnome.mutter.wayland.keybindings` and `org.gnome.settings-daemon.plugins.media-keys`; show an inline warning.

### 6.6 In-panel keys

Only while the panel is open: `Ctrl+1..9`, `Ctrl+PgUp/PgDn` (or GCH's `Ctrl+P/N`), `Delete`, a pin key, `Escape`. On GNOME 51+ attach a `Clutter.KeyController` to the menu box via `add_action()`; on 50, connect `key-press-event` on the menu actor or items (as Clipboard Indicator does). The search entry swallows printable keys, so letter shortcuts only work when focus is on the list.

---

## 7. UI quirks and hard parts

### 7.1 Paste-on-select

Setting the clipboard is easy; getting it into the right app is the hard part.

- Popup menus in the shell don't take window focus away from the app — closing the menu returns input to it. A full modal (`ModalDialog`, or a separate app window in Option D) does move focus; after closing it, re-activate the previous window with `Main.activateWindow(win)` if needed.
- **Synthesizing the paste works from inside the shell.** Clipboard Indicator creates a Clutter virtual keyboard and sends **Shift+Insert** (and **Ctrl+Shift+Insert** when the focused input's purpose is `TERMINAL`), ~50 ms after closing the menu. GNOME 51-compatible version:

```js
import Clutter from 'gi://Clutter';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

// enable()
const seat = global.stage.context.get_backend().get_default_seat(); // 51: no Clutter.get_default_backend()
this._vkbd = seat.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE);

_sendPaste() {
    const terminal = Main.inputMethod.content_purpose === Clutter.InputContentPurpose.TERMINAL;
    const keys = terminal
        ? [Clutter.KEY_Control_L, Clutter.KEY_Shift_L, Clutter.KEY_Insert]
        : [Clutter.KEY_Shift_L, Clutter.KEY_Insert];
    const t = Clutter.get_current_event_time() * 1000; // µs, as Clipboard Indicator does
    for (const k of keys)
        this._vkbd.notify_keyval(t, k, Clutter.KeyState.PRESSED);
    for (const k of [...keys].reverse())
        this._vkbd.notify_keyval(t, k, Clutter.KeyState.RELEASED);
}

// disable(): drop the reference; Clipboard Indicator calls run_dispose(), which EGO
// only allows with a comment explaining why it's necessary.
```

- Caveats: Shift+Insert isn't universal (some apps map it to PRIMARY or nothing); terminals detected via input purpose only if the terminal reports it; timing needs tuning. Make paste-on-select opt-in (Clipboard Indicator defaults it off).
- An out-of-process app (Option D) **cannot** inject keys on GNOME Wayland; it asks the extension to do it over D-Bus.

### 7.2 Where the UI lives

**Panel button** — `PanelMenu.Button` + `Main.panel.addToStatusArea(uuid, button)`. Most recognisable.

**Quick Settings** — a `QuickSettings.SystemIndicator` holding your toggles in `quickSettingsItems`, added with `Main.panel.statusArea.quickSettings.addExternalIndicator(indicator)`. Good home for a "Private mode" toggle; cramped for a history list.

**Modal overlay** — like Clipboard Indicator's image preview or a `ModalDialog`: real search entry, big list, preview pane. Most work, best ergonomics. v2.

**Separate app window** (Option D) — full GTK4 toolkit, but it takes focus (see §7.1).

### 7.3 Widgets and focus

- `St.Entry` for search; `entry.clutter_text.connect('text-changed', …)` for incremental search.
- **Focus timing:** grabbing focus on `open-state-changed` often fails because the menu isn't mapped yet; Clipboard Indicator uses a 50 ms `setTimeout`. Store that source ID and remove it in `disable()`.
- `St.ScrollView` with `overlay_scrollbars: true`; add the section's actor with `add_child()`.
- **No virtualised list in St.** Either paginate (GCH, 50-ish rows/page) or cap history and rely on scrolling (Clipboard Indicator, default 15). With large histories, paginate.
- **Images:** `St.ImageContent` (available since 45; `Clutter.Image` removed in 48). Since 48, `set_bytes()`/`set_data()` take a `Cogl.Context` first: `global.stage.context.get_backend().get_cogl_context()`. Simpler route for thumbnails: write a scaled PNG to `thumbs/` and show it with `St.Icon({gicon: Gio.FileIcon.new(file)})`. For decoding big images off the main thread, `GdkPixbuf.Pixbuf.new_from_stream_at_scale_async()` **[verify** that importing GdkPixbuf passes review; it is not one of the banned `Gdk`/`Gtk`/`Adw` imports**]**.
- **Style classes churn** (48 renamed `quick-menu-toggle` → `quick-toggle-has-menu`, `keyboard-subkeys` → `keyboard-subkeys-boxpointer`). Prefer your own classes in `stylesheet.css`.
- **Orientation:** use `orientation: Clutter.Orientation.VERTICAL`; `vertical: true` is gone in 51.

### 7.4 Miscellaneous

- Ellipsize any top-bar label; cap its length.
- RTL: `Clutter.ActorAlign`, not hard-coded sides.
- Notifications: default off for copies (Clipboard Indicator defaults `notify-on-copy` to false); respect Do Not Disturb.
- Explicit empty state ("Clipboard is empty").
- A11y: `accessible_name` on icon-only buttons; keyboard-only navigation; test with Orca.
- `session-modes`: **omit** the key (EGO requires dropping it when only `user` is used).

---

## 8. Edge cases and caveats

### Clipboard mechanics

1. **Own writes** — §2.4 flag + hash dedup.
2. **Source app exits** — Mutter keeps one plain type; you get `null` then a memory-source `owner-changed`. Dedup it.
3. **Apps that hoard or misbehave** — e.g. a July 2026 Fedora 44/GNOME 50 thread where apps stopped seeing each other's copies until logout; one participant traced a similar case to the Zed editor. Not your bug, but users will report it to you — have a diagnostic log toggle.
4. **Stale reads** — generation counter.
5. **Huge payloads** — per-entry cap (e.g. 10 MB) and a total cap. Mutter's own transfer caps are 4 MB text / 200 MB images.
6. **Empty / whitespace-only copies** — optionally skip.
7. **Non-UTF-8 bytes** — `TextDecoder` with `fatal: false`.
8. **Trailing NULs** in `text/plain`.
9. **Same content from different apps** — dedup by content hash.
10. **Copy storms** — debounce disk writes and UI rebuilds (Strata debounces capture by 50 ms).
11. **Slow sources** — `Meta.Selection` transfers time out after 15 s; your promise must handle a `null` result.

### Primary selection (middle-click)

12. Noisy — every text selection sets it. **Off by default**, opt-in.
13. GNOME proposed (Jan 2026, gsettings-desktop-schemas MR !119) disabling middle-click paste by default via `org.gnome.desktop.interface gtk-enable-primary-paste`, and Firefox discussed the same. Check your release's default with `gsettings get org.gnome.desktop.interface gtk-enable-primary-paste`. Either way, keep primary capture off by default.
14. Toolkit support for primary selection varies; test before building on it.

### Platform

15. **Wayland only.** No X11 session, no `Alt+F2 r`.
16. **XWayland apps are visible** to the extension (Mutter bridges their selections, advertising `UTF8_STRING`/`STRING`/`TEXT`). Include those types.
17. **Flatpak:** the extension is not a Flatpak. A Flatpak'd companion app (Option D) can talk to it over the session bus but cannot install it.
18. **Single-threaded JS.** Use Gio async I/O and chunk CPU work with `GLib.idle_add`; move real work to a daemon/app if needed.
19. **Breakage on GNOME updates.** Extensions touch internals; EGO requires `shell-version` to list only released versions (plus at most one development release). Port each spring and autumn.
20. **`disable()` must be perfect** — and is called on lock, logout, toggle and update. Disconnect every signal, remove every main-loop source (even ones that would remove themselves), destroy every actor, null every reference. Not `async` (throws on 51).
21. **Dev loop:** `gnome-shell --devkit` nested session, or log out/in.

### Security / privacy

22. You are harvesting secrets. Private mode, exclusions, password-manager hint, clear-on-lock, honest README.
23. `0700` directories, `0600` files (`Gio.FileCreateFlags.PRIVATE`).
24. EGO: declare clipboard access in the description; never share clipboard data with a third party without explicit user action; no telemetry.
25. Local-only is a feature; say so.

---

## 9. Performance

The failure that makes people uninstall: **the shell stutters as history grows** (the reason Strata and Ringboard exist).

- **Paginate** or cap the visible list.
- **O(1) dedup** via a hash → entry map. For strings, GCH hashes very long texts by length and short texts properly; for bytes, `GLib.compute_checksum_for_bytes(GLib.ChecksumType.SHA256, bytes)` is C-speed but still on the main thread — for multi-MB images consider hashing only size + a sampled prefix.
- **Doubly linked list** (GCH) or a sorted array with lazy re-sort for O(1) move-to-top/delete.
- **Debounce** disk writes, menu rebuilds and search.
- **Lazy thumbnails**, cached, never on the open path.
- **Search must not block**: chunk it, or do it in the daemon/app.
- **Measure:** Looking Glass (`Alt+F2` → `lg` still works; 51 adds a slowdown-factor debug flag), Sysprof, or `perf top -p $(pgrep -x gnome-shell)`.

---

## 10. Skeleton (Option A, GNOME 50/51)

### `metadata.json`

```json
{
  "uuid": "clip-history@yourname.github.io",
  "name": "Clip History",
  "description": "Clipboard history with search, pins and images.\n\nThis extension reads and stores what you copy (text and images) on this computer only. Nothing is sent over the network. Private mode and excluded apps stop recording.",
  "shell-version": ["50", "51"],
  "settings-schema": "org.gnome.shell.extensions.clip-history",
  "url": "https://github.com/yourname/gnome-clip-history"
}
```

- UUID namespace must be a domain/account you control (not `gnome.org`; `example.com` isn't yours).
- No `version` (EGO sets it), no `session-modes` (only `user`), no `gettext-domain` until you have translations.
- Only list versions you've actually tested.

### `extension.js`

```js
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';
import St from 'gi://St';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';
import * as PanelMenu from 'resource:///org/gnome/shell/ui/panelMenu.js';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

import {ClipboardWatcher} from './clipboard.js';
import {HistoryStore} from './store.js';

export default class ClipHistoryExtension extends Extension {
    enable() {
        this._settings = this.getSettings();
        this._logger = this.getLogger();

        const dataDir = GLib.build_filenamev([GLib.get_user_data_dir(), this.uuid]);
        this._store = new HistoryStore(dataDir, this._logger);
        this._store.load().catch(e => this._logger.error(e));

        this._watcher = new ClipboardWatcher(
            (mime, bytes) => this._store.add(mime, bytes), this._logger);

        this._button = new PanelMenu.Button(0.5, this.metadata.name, false);
        this._button.add_child(new St.Icon({
            icon_name: 'edit-paste-symbolic',
            style_class: 'system-status-icon',
        }));
        Main.panel.addToStatusArea(this.uuid, this._button);

        this._bindShortcut();
        this._shortcutChangedId = this._settings.connect('changed::show-history',
            () => this._bindShortcut());
    }

    disable() {
        // Runs on logout, extension toggle/update AND on every screen lock
        // (we only declare the default 'user' session mode). Must stay synchronous.
        this._settings.disconnect(this._shortcutChangedId);
        Main.wm.removeKeybinding('show-history');

        this._watcher.destroy();
        this._store.flushSync();   // final write; keep the index small so this is quick
        this._store.destroy();
        this._button.destroy();

        this._watcher = null;
        this._store = null;
        this._button = null;
        this._settings = null;
        this._logger = null;
    }

    _bindShortcut() {
        Main.wm.removeKeybinding('show-history'); // harmless if not bound
        Main.wm.addKeybinding('show-history', this._settings,
            Meta.KeyBindingFlags.IGNORE_AUTOREPEAT,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW | Shell.ActionMode.POPUP,
            () => this._button.menu.toggle());
    }
}
```

`HistoryStore` (not shown in full) owns: the in-memory list + hash map, `load()` (async), debounced async `writeIndex()` (§5.2), `flushSync()` using `replace_contents()` for the final write, and `destroy()` which removes its debounce source.

### Development commands

```bash
# live-edit install
ln -s "$PWD" ~/.local/share/gnome-shell/extensions/clip-history@yourname.github.io
glib-compile-schemas schemas/

# test in a nested shell (no logout needed)
dbus-run-session gnome-shell --devkit --wayland
#   …then, inside it:
gnome-extensions enable clip-history@yourname.github.io

# logs
journalctl -f -o cat /usr/bin/gnome-shell

# confirm the protocol situation (expect no output on GNOME)
sudo dnf install wayland-utils
wayland-info | grep -iE 'data_control'

# package / submit
gnome-extensions pack --extra-source=clipboard.js --extra-source=store.js
gnome-extensions upload --accept-tos        # GNOME 49+
```

---

## 11. Distribution

1. **extensions.gnome.org (EGO)** — the main channel. Review is manual and checks for malware/security issues and the rules, not bugs. The ones this project will hit:
   - Declare clipboard access in the description; no default clipboard shortcuts; no sharing clipboard data with third parties without explicit user action.
   - Create nothing before `enable()`; clean up everything in `disable()`; no binaries in the zip; no telemetry; no excessive logging; no deprecated modules (`Lang`, `Mainloop`, `ByteArray`).
   - **"Extensions must not be AI-generated."** Using AI as a learning aid or for completions is allowed, but submissions with unnecessary code, inconsistent style, **imaginary API usage**, or prompt-like comments get rejected. Revision 1 of this doc contained several imaginary APIs (see the revision notes) — exactly the pattern reviewers look for. Write the code yourself, and verify every API against the docs.
   - License: EGO distributes under terms compatible with GPL-2.0-or-later.
2. **GitHub source install** — keep a Makefile.
3. **Fedora packaging / COPR** — GPaste's extension is packaged in Fedora (`gnome-shell-extension-gpaste`); a COPR is a reasonable way to ship an Option B/D daemon or app alongside the extension.
4. **`shell-version`** is your compatibility contract: released versions only (plus at most one development release). Update it each cycle after testing.

---

## 12. Milestones

**M0 — Spike (half a day).** Extension that logs every `owner-changed`: type, `source` class, MIME list, byte size. Copy from GNOME Text Editor, Nautilus (files, then cut), Firefox, a screenshot, `wl-copy`, an XWayland app, and KeePassXC. Then quit the source app and watch Mutter's takeover emissions. Confirm: the `x-kde-passwordManagerHint` type is visible; `owner-changed` fires synchronously inside your own `set_content()`.

**M1 — MVP (2–3 days).** Text capture, dedup, capped list in a `PanelMenu.Button`, one user-set shortcut, restore on click, JSON persistence. Use it for a week.

**M2 — Usable (1 week).** Search, pins, delete, clear, private mode, excluded apps, secret-hint honouring, retention limits, images with lazy thumbnails, full keyboard nav, pagination.

**M3 — Native (1 week).** Paste-on-select (virtual keyboard, opt-in), paste-without-replacing, number keys, edit in place, clear on lock, Quick Settings private-mode toggle.

**M4 — Polish & ship (1–2 weeks).** Prefs with shortcut editor (Adw), a11y pass, perf pass, README with an honest privacy section, test on GNOME 50 **and** 51, EGO submission.

**M5 — Product architecture.** Move storage/search/UI to a GTK4 app or daemon (Option D/B) with a D-Bus contract; add encryption at rest and lock-surviving ephemeral history.

---

## 13. Open questions

- Is the end product a shell-native panel (A/B) or a native app window (D)? Your "Linux native app" goal points to D; the paste-into-previous-window UX points to a shell popup. A hybrid (shell popup for quick pick, app for management) is common.
- Images: store originals or transcode to a capped size on capture?
- File-list support: UX win, safety hazard (cut markers). Probably later, copy-only.
- Cross-desktop: on KDE/Sway/Hyprland, an Option D app could use `ext-data-control-v1` directly — the capture layer becomes pluggable (extension on GNOME, data-control elsewhere), as Ringboard's watcher split shows.
- EGO vs. COPR-first: affects how early the review bar matters.
- Ephemeral history vs. screen lock (§5.3): which behaviour do you promise users?

---

## 14. References

Platform & releases
- GNOME 51 release notes — https://release.gnome.org/51/ (developer notes: https://release.gnome.org/51/developers/)
- GNOME 50 release notes — https://release.gnome.org/50/
- Fedora 44 announcement — https://fedoramagazine.org/announcing-fedora-linux-44/
- Fedora 45 beta announcement — https://fedoramagazine.org/announcing-fedora-linux-45-beta/

Extension development
- GJS extension guide — https://gjs.guide/extensions/
- Porting guides — https://gjs.guide/extensions/upgrading/gnome-shell-51.html , …-50.html , …-49.html , …-48.html
- Review guidelines (EGO rules) — https://gjs.guide/extensions/review-guidelines/review-guidelines.html
- Session modes — https://gjs.guide/extensions/topics/session-modes.html
- Quick Settings — https://gjs.guide/extensions/topics/quick-settings.html
- `St.Clipboard` — https://gnome.pages.gitlab.gnome.org/gnome-shell/st/class.Clipboard.html
- `Meta.Selection` — https://gnome.pages.gitlab.gnome.org/mutter/meta/class.Selection.html
- `Meta.KeyBindingFlags` — https://gnome.pages.gitlab.gnome.org/mutter/meta/flags.KeyBindingFlags.html
- Mutter clipboard manager source — https://gitlab.gnome.org/GNOME/mutter/-/blob/main/src/core/meta-clipboard-manager.c
- Mutter selection source — https://gitlab.gnome.org/GNOME/mutter/-/blob/main/src/core/meta-selection.c

Protocols & portals
- `ext-data-control-v1` spec + compositor table — https://wayland.app/protocols/ext-data-control-v1
- XDG Clipboard portal — https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.Clipboard.html
- wl-clipboard man page — https://man.archlinux.org/man/wl-copy.1.en

Reference projects
- Clipboard Indicator (paste-on-select, excluded apps) — https://github.com/Tudmotu/gnome-shell-extension-clipboard-indicator
- Gnome Clipboard History (maintenance mode) — https://github.com/SUPERCILEX/gnome-clipboard-history ; design write-up: https://alexsaveau.dev/blog/gch
- Ringboard (daemon + Wayland/X11 watchers) — https://github.com/SUPERCILEX/clipboard-history
- Strata (Rust daemon + thin extension) — https://github.com/Edu4rdSHL/Strata ; write-up: https://edu4rdshl.dev/posts/rethinking-the-gnome-clipboard-issues/
- CopyQ known issues (GNOME extension, XWayland fallback) — https://copyq.readthedocs.io/en/latest/known-issues.html
- Copyous (active GNOME 48–50 extension) — https://extensions.gnome.org/extension/8834/copyous/
- Comparison of Linux clipboard managers (third-party, Aug 2026) — https://clipmer.app/blog/copyq-alternatives

Privacy
- Password-manager hint (Klipper origin) — https://phabricator.kde.org/D12539 ; KeePassXC implementation — https://github.com/keepassxreboot/keepassxc/blob/develop/src/gui/Clipboard.cpp

Primary selection
- gsettings-desktop-schemas MR !119 — https://gitlab.gnome.org/GNOME/gsettings-desktop-schemas/-/merge_requests/119

Troubleshooting
- Fedora 44 clipboard sync thread — https://discussion.fedoraproject.org/t/clipboard-stops-synchronizing-between-gui-applications-on-fedora-gnome-wayland/197016

**Confidence note:** Release dates, the compositor table, `St.Clipboard`/`Meta.Selection` signatures, Mutter's clipboard-manager and `set_owner` behaviour, EGO rules and porting-guide items were checked against primary sources on 25 Sep 2026. Items marked **[verify]** are reasonable inferences not confirmed by documentation; the M0 spike is designed to settle them.
