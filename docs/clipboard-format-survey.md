# Clipboard format survey — findings

Phase 1 survey of what real Windows apps put on the clipboard, captured with
the in-app survey tool (`src-tauri/src/survey.rs`) on Windows 11 Pro 26200,
2026-07-14. Raw log: `%APPDATA%\com.clipandcue.app\clipboard-survey.jsonl`
(local only, not committed).

Automation notes: Photoshop 2026 and Illustrator 2026 driven via COM with real
`Copy` commands; browser case is Chromium (Claude desktop + in-app browser);
plain text and file lists via PowerShell. Still wanted from a human: one
Explorer `Ctrl+C` on selected files (programmatic routes don't produce a real
Explorer copy) and desktop Word (not installed on this machine).

## Formats observed per source

### Photoshop 2026 (raster) — 19 formats

| Format | Size (sample) | Notes |
|---|---|---|
| `CF_DIB` / `CF_DIBV5` | 360 KB | plain raster, always rendered |
| `CF_BITMAP`, `CF_PALETTE`, `CF_METAFILEPICT`, `CF_ENHMETAFILE` | — | GDI handles, not HGLOBAL |
| `Photoshop Paste in Place` | 28 B | position metadata |
| `Photoshop DIB Layer`, `Photoshop DIB Layer X`, `Adobe Photoshop Image`, `Photoshop Clip Source` | failed | **delayed render; GetClipboardData failed cross-process** |
| `Embed Source`, `Native`, `Object Descriptor` (OLE) | 31 KB | OLE embedding of the PSD content |
| `Chromium Web Custom MIME Data Format`, `com.adobe.uxp.drag-drop-dummy` | failed | UXP internals |

### Illustrator 2026 (vector) — 17 formats

| Format | Size (sample) | Notes |
|---|---|---|
| `Adobe Illustrator PGF 14.0` | 494 KB | **the editable-paste format** |
| `ADOBE AI3` / `Encapsulated PostScript` | 45 KB | legacy vector |
| `Portable Document Format` | 891 KB | full PDF, largest payload |
| `Scalable Vector Graphics`, `image/svg+xml`, `…For Adobe Muse` | <1 KB | SVG variants |
| `PNG` | 3 KB | raster preview |
| `CF_DIB` / `CF_DIBV5` | 163–199 KB | raster fallback |
| `Object Descriptor`, `Ole Private Data`, `DataObject` | small | OLE plumbing |

**Illustrator only writes the clipboard when the app loses focus.** A copy
inside AI stays app-internal until deactivate, then the full 17-format flush
fires. Capture must simply react to the (late) update; nothing special needed,
but don't expect the clip at Ctrl+C time.

### Chromium browsers (Claude desktop, in-app browser; Chrome/Edge same engine)

`HTML Format` (CF_HTML with source URL context), `CF_UNICODETEXT`, `CF_TEXT`,
`CF_OEMTEXT`, `CF_LOCALE`, plus Chromium-internal `source RFH token` and
`source URL` registered formats. RFH token references a live renderer process
— stale after the tab closes; likely skip on paste-back (needs testing).

### File lists

PowerShell `Set-Clipboard -Path` (2 files): `CF_HDROP` (616 B), `FileNameW`,
`FileName`, `Ole Private Data`. Real Explorer copies add more (Shell IDList
Array, Preferred DropEffect, etc.) — pending a manual Explorer Ctrl+C to
capture; `Preferred DropEffect` matters for paste-back copy-vs-cut semantics.

### Plain writers (PowerShell/WinForms)

Text: `CF_UNICODETEXT` + synthesized `CF_TEXT`/`CF_OEMTEXT`/`CF_LOCALE`.
Bitmap: `System.Drawing.Bitmap`, `CF_BITMAP` (GDI), `CF_DIB`, `CF_DIBV5`.

## Engine design consequences (bake these in)

1. **Never race the writer.** Opening the clipboard too soon after
   `WM_CLIPBOARDUPDATE` makes the WRITER'S next operation fail — reproduced
   twice (PowerShell `Set-Clipboard` "operation did not succeed"; WinForms
   `SetImage` failing on its OLE flush). The capture engine must delay
   ~150 ms after the update, then open with escalating-backoff retries, and
   hold the clipboard as briefly as possible. A clipboard manager that breaks
   copying is worse than no clipboard manager.
2. **Dedupe by sequence number, coalesce by time.** Every OLE writer fired
   2–3 `WM_CLIPBOARDUPDATE` for one copy (same
   `GetClipboardSequenceNumber`), and multi-pass writers produce multiple
   sequences per user action (first pass had only `DataObject` + OLE plumbing,
   ~150 ms before the real formats). Confirms the brief: sequence-based
   suppression + ~1 s coalescing window.
3. **Per-format capture failure is normal.** Photoshop's private delayed-render
   formats refuse cross-process `GetClipboardData`. Capture must be
   best-effort per format: store what renders, record what didn't, never drop
   the whole clip. (Photoshop paste-back fidelity via the OLE `Embed Source`
   + DIB set needs a dedicated paste test.)
4. **GDI-handle formats need special handling.** `CF_BITMAP`, `CF_PALETTE`,
   `CF_METAFILEPICT`, `CF_ENHMETAFILE` are not HGLOBAL bytes. A DIB variant
   coexisted in every observed case, so v1 can store DIB/DIBV5 and re-offer
   those, letting Windows synthesize the GDI forms on paste.
5. **Sizes are real but manageable.** A trivial rect+star in AI produced
   ~1.6 MB across formats (PDF 891 KB + PGF 494 KB + rasters). Design-app
   clips of real artwork will hit tens of MB; the 50 MB per-format cap and
   size accounting in the brief are the right shape.
6. **Source attribution needs a fallback.** `GetClipboardOwner` is often null
   or unhelpful (OLE writers); foreground-window exe is the practical
   fallback (`source_via` field distinguishes them), excluding our own exe.
7. **Never restore OLE bookkeeping formats.** Restoring captured
   `DataObject` / `Ole Private Data` poisons OLE's synthesized clipboard
   view: consumers that paste via `OleGetClipboard` (Office, Adobe, .NET)
   then see 1 of 6 formats. Verified empirically: skipping them on restore
   (capture still records them) makes all content formats visible to OLE
   consumers. Raw `EnumClipboardFormats` sees everything either way — test
   paste fidelity through OLE, not just Win32.
