# clipandcue for PC — project brief

You are picking up a new project: **clipandcue for Windows (and later Linux)**.
This is a ground-up rewrite of the macOS app [clipandcue](https://github.com/mrmichaelmoorecom-sys/clipandcue)
(clipandcue.com) — a clipboard history manager whose signature feature is
**full-fidelity clipboard capture**: it snapshots every format on the clipboard
so design-app content pastes back *editable* (an Office shape comes back a
shape, an Illustrator path comes back vector art), not as a flattened image.

The Mac app is the product-behavior reference. Zero code ports; the concept and
the lessons below are what carry over.

## Mission

Windows-first MVP, Linux later (X11 first, Wayland best-effort). Ship the same
soul: tiny, native-feeling, local-only, no accounts, no telemetry, free
(CC BY-NC 4.0, LICENSE in repo).

## Stack (decided)

- **Tauri v2** — Rust core + webview UI. Target < 15 MB installed. Do NOT use
  Electron (bloat contradicts the product ethos, and we have battle scars).
- Rust clipboard engine using the `windows` crate directly. Generic clipboard
  crates (arboard etc.) are format-lossy — the whole product is format fidelity,
  so own this layer.
- Storage: local app-data dir, JSON index + per-clip blob files (mirrors the
  Mac layout). No database needed at this scale (50-item cap).

## Product spec (MVP)

1. **Tray icon + dropdown** listing recent clips (default 9, max 50), newest
   first. Pinned clips stay on top.
2. **Capture**: listen via `AddClipboardFormatListener` (event-driven; do not
   poll). On change, snapshot ALL formats: enumerate `EnumClipboardFormats`,
   store bytes per format id/name. Cap per-format size (default 50 MB,
   configurable 1–200) with a "too big, not saved" tray flash + notification.
3. **Paste-back**: clicking a clip restores EVERY captured format via
   `SetClipboardData`, then (if auto-paste enabled) focuses the previous
   foreground window and synthesizes Ctrl+V with `SendInput`. No special
   permission needed on Windows.
4. **Quick-paste HUD**: global hotkey (default Ctrl+Alt+V, user-configurable
   via `RegisterHotKey`) opens a numbered list; hitting 1–9 pastes that clip
   into the previously focused window. The HUD must NOT steal focus from the
   target window's text caret (use a non-activating window style;
   WS_EX_NOACTIVATE).
5. **Pins**, **plain-text paste toggle**, **clear history**, **clear on quit**,
   **launch at login** (registry Run key), Preferences window.
6. **Privacy defaults**: respect clipboard-manager exclusion formats —
   `ExcludeClipboardContentFromMonitorProcessing`, `CanIncludeInClipboardHistory`
   =0, and the cloud-exclusion format. Password managers set these; skip those
   copies by default (toggleable). Ship an honest privacy page later
   (clipandcue.com/privacy is the template; nothing is collected, everything
   local).

## Lessons from the Mac version — bake these in from day one

These were all shipped bugs on Mac. Do not rediscover them.

- **Self-paste suppression must be sequence-based, not counter-based.** The Mac
  app suppressed its own pasteboard writes with a counter that raced the
  polling loop and silently swallowed the user's next real copy ~55% of the
  time after multi-item pastes. On Windows, compare `GetClipboardSequenceNumber`
  before/after your own writes and ignore exactly those sequence numbers.
- **Screenshot tools write the clipboard in multiple passes.** Snipping Tool /
  Win+Shift+S can fire multiple updates for one screenshot. Collapse image
  clips arriving within ~1s of each other into one history entry (replace, not
  stack).
- **Pinned items must never brick capture.** Mac bug: when pins filled the
  history cap, every new copy was inserted below the pinned block and instantly
  evicted — capture silently died. Enforce cap on unpinned items only, and
  never let pin count reach the cap (warn or grow).
- **Drag-and-drop format ordering matters and differs per receiver.** Native
  apps read the richest format first; browser/Electron apps (Slack, Discord,
  Claude) only accept file-backed drops (`CF_HDROP` with a real temp file).
  On Mac we never fully solved Electron image drops. On Windows, OLE
  drag-drop with IDataObject offering BOTH native formats and a temp-file
  CF_HDROP is the plan — test against Slack + a browser EARLY, this cost us
  a week of shipped regressions on Mac. Keep drag-out un-advertised until it's
  proven across receivers.
- **One feature per release, verify on a clean machine before shipping.** The
  Mac changelog has a v0.2.10→v0.2.17 scar tissue stretch from stacking
  speculative fixes.
- Version strings: max three dot-separated integers (Apple rejected 0.2.9.1;
  keep the habit everywhere for sanity).

## Explicitly out of scope for v1

- Sync (CloudKit is Apple-only; local-only is also the privacy story). A
  file-based sync (user-chosen folder) MAY come later.
- Linux (X11 via `x11-clipboard`-style TARGETS snapshot later; Wayland only
  where wlr-data-control exists — stock GNOME can't support background
  clipboard managers; document, don't fight it).
- Stacks/groups, file-clip previews, OCR/AI anything.
- Auto-update (ship winget + direct download; revisit updater later — on Mac,
  an update button caused an App Store rejection, 2.4.5(vii); irrelevant on
  Windows direct-distribution but keep the lesson in mind for MS Store builds).

## Distribution plan (later, not MVP)

Direct download (code-signed when Mike sets up a cert — unsigned triggers
SmartScreen warnings at first; reputation builds), then winget manifest, then
Microsoft Store (MSIX) if desired — its review is far gentler than Apple's.

## Working process (standing rules)

- Mike personally clicks any final Submit/Send on anything outward-facing
  (store submissions, forms, published posts). Prepare everything; stop before
  the last click.
- Public-facing copy in Mike's voice: no em-dashes ever, plain and direct,
  factual claims must be literally true ("built by me with AI tools", never
  "hand-written"). Commits carry `Co-Authored-By:` trailers as usual.
- Don't commit binaries, certs, or working logs to this public repo.
- The Mac apps (clipandcue, clipandnote) live on Mike's Mac with their own
  pipelines; this repo is Windows/Linux only.

## Suggested phase 1 (first session on the PC)

1. Scaffold Tauri v2 app on the Windows machine (`npm create tauri-app`),
   confirm tray icon + global hotkey + a hello-world dropdown.
2. Rust: clipboard listener + full-format snapshot into memory, log what
   formats real apps produce (copy from Word, Illustrator, Photoshop, a
   browser, Explorer files) — this survey drives everything.
3. Persist history to disk, render the dropdown list with text/image previews.
4. Paste-back (all formats) + Ctrl+V synthesis + sequence-number suppression.
5. HUD + numbered quick paste.

Then iterate with Mike testing against his real design apps, same as we did
on Mac.
