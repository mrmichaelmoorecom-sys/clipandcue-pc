import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

interface FormatEntry {
  id: number;
  id_hex: string;
  name: string;
  kind: "standard" | "registered" | "private";
  size_bytes: number | null;
  note: string | null;
}

interface SurveyEvent {
  timestamp: string;
  sequence: number;
  source_exe: string | null;
  formats: FormatEntry[];
}

function fmtSize(bytes: number | null): string {
  if (bytes === null) return "—";
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

function render(list: HTMLElement, ev: SurveyEvent) {
  list.querySelector(".empty")?.remove();

  const li = document.createElement("li");
  li.className = "clip survey";

  const header = document.createElement("div");
  header.className = "survey-header";
  header.textContent = `${ev.source_exe ?? "unknown source"} — ${ev.formats.length} format${ev.formats.length === 1 ? "" : "s"} (seq ${ev.sequence})`;
  li.appendChild(header);

  const table = document.createElement("ul");
  table.className = "format-list";
  for (const f of ev.formats) {
    const row = document.createElement("li");
    row.className = `format ${f.kind}`;
    row.textContent = `${f.name} (${f.id_hex}) · ${fmtSize(f.size_bytes)}${f.note ? ` · ${f.note}` : ""}`;
    table.appendChild(row);
  }
  li.appendChild(table);

  list.prepend(li);
  // Keep the hello-world window light; the JSONL file has the full record.
  while (list.children.length > 20) list.lastElementChild?.remove();
}

window.addEventListener("DOMContentLoaded", () => {
  const list = document.getElementById("clip-list")!;

  listen<SurveyEvent>("clipboard-survey", (e) => render(list, e.payload));

  // Dismiss on Escape, like a native menu.
  window.addEventListener("keydown", (e) => {
    if (e.key === "Escape") getCurrentWindow().hide();
  });
});
