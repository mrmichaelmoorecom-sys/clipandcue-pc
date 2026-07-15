import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

type ClipKind = "text" | "image" | "files" | "other";

interface FormatMeta {
  id: number;
  name: string;
  size: number;
}

interface ClipMeta {
  id: string;
  ts_ms: number;
  source_exe: string | null;
  pinned: boolean;
  kind: ClipKind;
  preview_text: string | null;
  preview_image: string | null;
  formats: FormatMeta[];
  hash: number;
}

interface Settings {
  show_count: number;
  history_cap: number;
  max_format_mb: number;
  auto_paste: boolean;
  plain_text_paste: boolean;
  skip_excluded: boolean;
  clear_on_quit: boolean;
  launch_at_login: boolean;
  hotkey: string;
  survey_log: boolean;
}

const app = document.getElementById("app")!;
const label = getCurrentWindow().label;

/* ---------- shared helpers ---------- */

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  cls?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (text !== undefined) e.textContent = text;
  return e;
}

function timeAgo(ts: number): string {
  const s = Math.max(0, (Date.now() - ts) / 1000);
  if (s < 60) return "now";
  if (s < 3600) return `${Math.floor(s / 60)}m`;
  if (s < 86400) return `${Math.floor(s / 3600)}h`;
  return `${Math.floor(s / 86400)}d`;
}

function sourceName(exe: string | null): string {
  if (!exe) return "";
  return exe.replace(/\.exe$/i, "");
}

/* ---------- dropdown window ---------- */

const previewCache = new Map<string, string>();

async function imagePreviewUrl(id: string): Promise<string | null> {
  if (previewCache.has(id)) return previewCache.get(id)!;
  const p = await invoke<{ mime: string; b64: string } | null>("get_preview", { id });
  if (!p) return null;
  const url = `data:${p.mime};base64,${p.b64}`;
  previewCache.set(id, url);
  if (previewCache.size > 60) {
    const first = previewCache.keys().next().value;
    if (first) previewCache.delete(first);
  }
  return url;
}

function renderDropdown(clips: ClipMeta[], settings: Settings, hud: boolean) {
  app.innerHTML = "";

  const header = el("header", "titlebar");
  header.appendChild(el("span", "brand", "clipandcue"));
  const right = el("span", "header-right");
  if (settings.plain_text_paste) right.appendChild(el("span", "mode-badge", "TXT"));
  const gear = el("button", "icon-btn", "⚙");
  gear.title = "Preferences";
  gear.addEventListener("click", () => invoke("open_prefs"));
  right.appendChild(gear);
  right.appendChild(el("span", "hint", settings.hotkey.replace(/\b\w/g, (c) => c.toUpperCase())));
  header.appendChild(right);
  app.appendChild(header);

  const list = el("ul", "clip-list");
  list.id = "clip-list";

  if (clips.length === 0) {
    list.appendChild(el("li", "clip empty", "Nothing copied yet"));
  }

  clips.forEach((clip, i) => {
    const li = el("li", "clip" + (clip.pinned ? " pinned" : ""));

    const num = el("span", "num", i < 9 ? String(i + 1) : "");
    li.appendChild(num);

    const body = el("div", "clip-body");
    if (clip.kind === "image") {
      const img = el("img", "thumb") as HTMLImageElement;
      img.alt = "image clip";
      imagePreviewUrl(clip.id).then((u) => {
        if (u) img.src = u;
        else img.replaceWith(el("div", "clip-text", "[image]"));
      });
      body.appendChild(img);
    } else {
      const text =
        clip.preview_text ??
        (clip.kind === "files" ? "[files]" : `[${clip.formats.length} formats]`);
      body.appendChild(el("div", "clip-text", text));
    }
    const meta = el("div", "clip-meta");
    const kindIcon = { text: "📝", image: "🖼", files: "📁", other: "📦" }[clip.kind];
    meta.textContent = `${kindIcon} ${sourceName(clip.source_exe)} · ${timeAgo(clip.ts_ms)}`;
    body.appendChild(meta);
    li.appendChild(body);

    const actions = el("div", "actions");
    const pin = el("button", "icon-btn pin", clip.pinned ? "📌" : "📍");
    pin.title = clip.pinned ? "Unpin" : "Pin";
    pin.addEventListener("click", (e) => {
      e.stopPropagation();
      invoke("toggle_pin", { id: clip.id });
    });
    const del = el("button", "icon-btn del", "✕");
    del.title = "Delete";
    del.addEventListener("click", (e) => {
      e.stopPropagation();
      invoke("delete_clip", { id: clip.id });
    });
    actions.appendChild(pin);
    actions.appendChild(del);
    li.appendChild(actions);

    li.addEventListener("click", () => invoke("paste_clip", { id: clip.id }));
    list.appendChild(li);
  });
  app.appendChild(list);

  const footer = el("footer", "footer");
  const count = el("span", "hint", `${clips.length} clip${clips.length === 1 ? "" : "s"}`);
  footer.appendChild(count);
  const clear = el("button", "clear-btn", "Clear history");
  let armed = false;
  clear.addEventListener("click", () => {
    if (!armed) {
      armed = true;
      clear.textContent = "Really clear?";
      clear.classList.add("armed");
      setTimeout(() => {
        armed = false;
        clear.textContent = "Clear history";
        clear.classList.remove("armed");
      }, 2500);
    } else {
      invoke("clear_history");
      armed = false;
      clear.textContent = "Clear history";
      clear.classList.remove("armed");
    }
  });
  footer.appendChild(clear);
  app.appendChild(footer);

  document.body.classList.toggle("hud", hud);
}

let selectedIndex = -1;

function applySelection(clips: ClipMeta[]) {
  const items = document.querySelectorAll<HTMLElement>("#clip-list .clip:not(.empty)");
  items.forEach((li, i) => li.classList.toggle("selected", i === selectedIndex));
  if (selectedIndex >= 0 && items[selectedIndex]) {
    items[selectedIndex].scrollIntoView({ block: "nearest" });
  }
  void clips;
}

async function bootDropdown() {
  let hud = false;
  let currentClips: ClipMeta[] = [];
  const refresh = async () => {
    const [clips, settings] = await Promise.all([
      invoke<ClipMeta[]>("list_clips"),
      invoke<Settings>("get_settings"),
    ]);
    currentClips = clips;
    selectedIndex = -1;
    renderDropdown(clips, settings, hud);
  };

  await listen<ClipMeta[]>("history-updated", async (e) => {
    const settings = await invoke<Settings>("get_settings");
    currentClips = e.payload;
    selectedIndex = -1;
    renderDropdown(e.payload, settings, hud);
  });
  await listen<boolean>("dropdown-shown", (e) => {
    hud = e.payload;
    refresh();
  });

  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") {
      invoke("hide_window");
    } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      const max = currentClips.length - 1;
      if (max < 0) return;
      selectedIndex =
        e.key === "ArrowDown"
          ? Math.min(selectedIndex + 1, max)
          : Math.max(selectedIndex - 1, 0);
      applySelection(currentClips);
    } else if (e.key === "Enter" && selectedIndex >= 0 && currentClips[selectedIndex]) {
      invoke("paste_clip", { id: currentClips[selectedIndex].id });
    } else if (/^[1-9]$/.test(e.key) && currentClips[Number(e.key) - 1]) {
      invoke("paste_clip", { id: currentClips[Number(e.key) - 1].id });
    }
  });

  await refresh();
}

/* ---------- preferences window ---------- */

function prefRow(labelText: string, input: HTMLElement, note?: string): HTMLElement {
  const row = el("label", "pref-row");
  const span = el("span", "pref-label", labelText);
  row.appendChild(span);
  row.appendChild(input);
  if (note) row.appendChild(el("small", "pref-note", note));
  return row;
}

function numberInput(value: number, min: number, max: number): HTMLInputElement {
  const i = el("input") as HTMLInputElement;
  i.type = "number";
  i.min = String(min);
  i.max = String(max);
  i.value = String(value);
  return i;
}

function checkbox(value: boolean): HTMLInputElement {
  const i = el("input") as HTMLInputElement;
  i.type = "checkbox";
  i.checked = value;
  return i;
}

async function bootPrefs() {
  const s = await invoke<Settings>("get_settings");
  app.innerHTML = "";
  app.appendChild(el("h1", "prefs-title", "Preferences"));

  const form = el("div", "prefs-form");

  const cap = numberInput(s.history_cap, 1, 50);
  const maxMb = numberInput(s.max_format_mb, 1, 200);
  const autoPaste = checkbox(s.auto_paste);
  const plain = checkbox(s.plain_text_paste);
  const skipExcluded = checkbox(s.skip_excluded);
  const clearQuit = checkbox(s.clear_on_quit);
  const login = checkbox(s.launch_at_login);
  const survey = checkbox(s.survey_log);

  const hotkey = el("input", "hotkey-input") as HTMLInputElement;
  hotkey.value = s.hotkey;
  hotkey.readOnly = true;
  hotkey.addEventListener("keydown", (e) => {
    e.preventDefault();
    if (["Control", "Alt", "Shift", "Meta"].includes(e.key)) return;
    const parts: string[] = [];
    if (e.ctrlKey) parts.push("ctrl");
    if (e.altKey) parts.push("alt");
    if (e.shiftKey) parts.push("shift");
    if (e.metaKey) parts.push("super");
    if (parts.length === 0) return; // require a modifier
    parts.push(e.key.length === 1 ? e.key.toLowerCase() : e.key);
    hotkey.value = parts.join("+");
  });

  form.appendChild(prefRow("History size", cap, "unpinned clips kept, max 50"));
  form.appendChild(prefRow("Max size per format (MB)", maxMb, "larger copies are not saved"));
  form.appendChild(prefRow("Auto-paste on select", autoPaste));
  form.appendChild(prefRow("Plain-text paste", plain, "paste text only, no formatting"));
  form.appendChild(prefRow("Skip password-manager copies", skipExcluded));
  form.appendChild(prefRow("Clear history on quit", clearQuit));
  form.appendChild(prefRow("Launch at login", login));
  form.appendChild(prefRow("Hotkey", hotkey, "click, then press the new combination"));
  form.appendChild(prefRow("Log format survey (diagnostics)", survey));

  const save = el("button", "save-btn", "Save");
  const status = el("span", "save-status", "");
  save.addEventListener("click", async () => {
    const next: Settings = {
      show_count: 9,
      history_cap: Math.min(50, Math.max(1, Number(cap.value) || 50)),
      max_format_mb: Math.min(200, Math.max(1, Number(maxMb.value) || 50)),
      auto_paste: autoPaste.checked,
      plain_text_paste: plain.checked,
      skip_excluded: skipExcluded.checked,
      clear_on_quit: clearQuit.checked,
      launch_at_login: login.checked,
      hotkey: hotkey.value,
      survey_log: survey.checked,
    };
    try {
      await invoke("set_settings", { settings: next });
      status.textContent = "Saved";
    } catch (err) {
      status.textContent = String(err);
    }
    setTimeout(() => (status.textContent = ""), 2500);
  });

  const footer = el("div", "prefs-footer");
  footer.appendChild(save);
  footer.appendChild(status);

  app.appendChild(form);
  app.appendChild(footer);
}

/* ---------- boot ---------- */

window.addEventListener("DOMContentLoaded", () => {
  if (label === "prefs") bootPrefs();
  else bootDropdown();
});
