//! Clip history: in-memory list + on-disk layout mirroring the Mac app.
//!
//! Layout under the app data dir:
//!   history.json            — ordered index of clip metadata (newest first)
//!   clips/<id>/<fmt>.bin    — raw bytes per captured format
//!   clips/<id>/preview.png|bmp — optional image preview

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ClipKind {
    Text,
    Image,
    Files,
    Other,
    /// User-assembled stack (drag one row onto another). One level deep;
    /// children hold their own formats and paste sequentially.
    Group,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct FormatMeta {
    pub id: u32,
    pub name: String,
    pub size: u64,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct ClipMeta {
    pub id: String,
    pub ts_ms: u64,
    pub source_exe: Option<String>,
    pub pinned: bool,
    pub kind: ClipKind,
    /// First ~200 chars of text, or file names for Files clips.
    pub preview_text: Option<String>,
    /// preview.png / preview.bmp exists in the clip dir.
    pub preview_image: Option<String>,
    pub formats: Vec<FormatMeta>,
    /// Content hash across all format bytes, for consecutive-dup detection.
    pub hash: u64,
    /// Group children, in paste order. Empty for non-groups.
    #[serde(default)]
    pub children: Vec<ClipMeta>,
}

/// Raw captured format, only held in memory between capture and persist.
pub struct RawFormat {
    pub id: u32,
    pub name: String,
    pub bytes: Vec<u8>,
}

#[derive(Serialize, Deserialize, Default)]
struct Index {
    clips: Vec<ClipMeta>,
}

pub struct History {
    dir: PathBuf,
    clips: Vec<ClipMeta>, // newest first
}

impl History {
    pub fn load(dir: PathBuf) -> Self {
        let clips = std::fs::read_to_string(dir.join("history.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Index>(&s).ok())
            .map(|i| i.clips)
            .unwrap_or_default();
        Self { dir, clips }
    }

    fn save_index(&self) {
        let _ = std::fs::create_dir_all(&self.dir);
        if let Ok(json) = serde_json::to_string(&Index { clips: self.clips.clone() }) {
            let _ = std::fs::write(self.dir.join("history.json"), json);
        }
    }

    pub fn clip_dir(&self, id: &str) -> PathBuf {
        self.dir.join("clips").join(id)
    }

    /// Pinned first (their own recency order), then unpinned newest-first.
    pub fn view(&self) -> Vec<ClipMeta> {
        let mut v: Vec<ClipMeta> = self.clips.iter().filter(|c| c.pinned).cloned().collect();
        v.extend(self.clips.iter().filter(|c| !c.pinned).cloned());
        v
    }

    pub fn get(&self, id: &str) -> Option<&ClipMeta> {
        self.clips.iter().find(|c| c.id == id)
    }

    /// Find a clip anywhere: top level or inside a group.
    pub fn find(&self, id: &str) -> Option<&ClipMeta> {
        self.clips.iter().find_map(|c| {
            if c.id == id {
                Some(c)
            } else {
                c.children.iter().find(|k| k.id == id)
            }
        })
    }

    /// Drag-to-group, Mac semantics: dropping onto an existing group appends
    /// the dropped clip('s children); otherwise a new group forms in the
    /// target's slot with children [target, dropped], inheriting the
    /// target's pinned state. One level deep — dropped groups flatten in.
    pub fn group(&mut self, dropped_id: &str, target_id: &str) -> bool {
        if dropped_id == target_id {
            return false;
        }
        let Some(dropped_pos) = self.clips.iter().position(|c| c.id == dropped_id) else {
            return false;
        };
        if !self.clips.iter().any(|c| c.id == target_id) {
            return false;
        }
        let mut dropped = self.clips.remove(dropped_pos);
        let target_pos = self.clips.iter().position(|c| c.id == target_id).unwrap();

        let dropped_children = if dropped.kind == ClipKind::Group {
            std::mem::take(&mut dropped.children)
        } else {
            dropped.pinned = false;
            vec![dropped]
        };

        let target = &mut self.clips[target_pos];
        if target.kind == ClipKind::Group {
            target.children.extend(dropped_children);
        } else {
            let mut old_target = target.clone();
            old_target.pinned = false;
            let group = ClipMeta {
                id: format!("group-{}", old_target.id),
                ts_ms: old_target.ts_ms,
                source_exe: None,
                pinned: target.pinned,
                kind: ClipKind::Group,
                preview_text: None,
                preview_image: None,
                formats: Vec::new(),
                hash: 0,
                children: {
                    let mut kids = vec![old_target];
                    kids.extend(dropped_children);
                    kids
                },
            };
            self.clips[target_pos] = group;
        }
        self.save_index();
        true
    }

    /// Flatten a group back into top-level rows at its position. Children
    /// come back unpinned.
    pub fn ungroup(&mut self, id: &str) -> bool {
        let Some(pos) = self.clips.iter().position(|c| c.id == id && c.kind == ClipKind::Group)
        else {
            return false;
        };
        let group = self.clips.remove(pos);
        for (i, mut kid) in group.children.into_iter().enumerate() {
            kid.pinned = false;
            self.clips.insert(pos + i, kid);
        }
        self.save_index();
        true
    }

    pub fn newest_unpinned(&self) -> Option<&ClipMeta> {
        self.clips.iter().find(|c| !c.pinned)
    }

    /// Insert a new clip: write blobs, add to index, evict over-cap unpinned.
    pub fn insert(&mut self, meta: ClipMeta, blobs: &[RawFormat], preview_png: Option<Vec<u8>>, cap: usize) {
        let dir = self.clip_dir(&meta.id);
        let _ = std::fs::create_dir_all(&dir);
        for f in blobs {
            let _ = std::fs::write(dir.join(format!("{}.bin", f.id)), &f.bytes);
        }
        if let (Some(bytes), Some(name)) = (preview_png.as_ref(), meta.preview_image.as_ref()) {
            let _ = std::fs::write(dir.join(name), bytes);
        }
        self.clips.insert(0, meta);

        // Cap applies to unpinned only — pinned clips can never evict-starve
        // capture (Mac lesson).
        let unpinned: Vec<String> = self
            .clips
            .iter()
            .filter(|c| !c.pinned)
            .skip(cap)
            .map(|c| c.id.clone())
            .collect();
        for id in unpinned {
            self.remove_files(&id);
            self.clips.retain(|c| c.id != id);
        }
        self.save_index();
    }

    /// Replace the newest unpinned clip (multi-pass screenshot collapse).
    pub fn replace_newest_unpinned(&mut self, meta: ClipMeta, blobs: &[RawFormat], preview_png: Option<Vec<u8>>, cap: usize) {
        if let Some(old_id) = self.newest_unpinned().map(|c| c.id.clone()) {
            self.remove_files(&old_id);
            self.clips.retain(|c| c.id != old_id);
        }
        self.insert(meta, blobs, preview_png, cap);
    }

    fn remove_files(&self, id: &str) {
        // Groups own no blobs themselves, but their children do.
        if let Some(meta) = self.clips.iter().find(|c| c.id == id) {
            for kid in &meta.children {
                let _ = std::fs::remove_dir_all(self.clip_dir(&kid.id));
            }
        }
        let _ = std::fs::remove_dir_all(self.clip_dir(id));
    }

    /// Move a clip to `target_view_index` (an index into view() order).
    /// Dropping inside the pinned block pins it; below the block unpins it.
    /// A pinned clip dropped right at the boundary stays pinned (move to end
    /// of the pinned group).
    pub fn reorder(&mut self, id: &str, target_view_index: usize) -> bool {
        let Some(pos) = self.clips.iter().position(|c| c.id == id) else {
            return false;
        };
        let mut dragged = self.clips.remove(pos);
        let mut combined: Vec<ClipMeta> = self.clips.iter().filter(|c| c.pinned).cloned().collect();
        let pinned_count = combined.len();
        combined.extend(self.clips.iter().filter(|c| !c.pinned).cloned());
        let idx = target_view_index.min(combined.len());
        dragged.pinned = idx < pinned_count || (idx == pinned_count && dragged.pinned);
        combined.insert(idx, dragged);
        self.clips = combined;
        self.save_index();
        true
    }

    pub fn set_pinned(&mut self, id: &str, pinned: bool) -> bool {
        if let Some(c) = self.clips.iter_mut().find(|c| c.id == id) {
            c.pinned = pinned;
            self.save_index();
            true
        } else {
            false
        }
    }

    pub fn delete(&mut self, id: &str) {
        if self.clips.iter().any(|c| c.id == id) {
            self.remove_files(id);
            self.clips.retain(|c| c.id != id);
        } else {
            // A child inside a group: remove it (and its blobs); a group
            // left with one child flattens, with zero children disappears.
            let dir = self.clip_dir(id);
            for g in self.clips.iter_mut().filter(|c| c.kind == ClipKind::Group) {
                g.children.retain(|k| k.id != id);
            }
            let _ = std::fs::remove_dir_all(dir);
            let mut flattened: Vec<ClipMeta> = Vec::new();
            self.clips.retain_mut(|c| {
                if c.kind != ClipKind::Group {
                    return true;
                }
                match c.children.len() {
                    0 => false,
                    1 => {
                        let mut kid = c.children.remove(0);
                        kid.pinned = c.pinned;
                        flattened.push(kid);
                        false
                    }
                    _ => true,
                }
            });
            // Flattened singles go back on top of the unpinned block.
            for kid in flattened {
                self.clips.insert(0, kid);
            }
        }
        self.save_index();
    }

    pub fn clear(&mut self, keep_pinned: bool) {
        let doomed: Vec<String> = self
            .clips
            .iter()
            .filter(|c| !(keep_pinned && c.pinned))
            .map(|c| c.id.clone())
            .collect();
        for id in &doomed {
            self.remove_files(id);
        }
        self.clips.retain(|c| !doomed.contains(&c.id));
        self.save_index();
    }

    pub fn load_blob(&self, id: &str, format_id: u32) -> Option<Vec<u8>> {
        std::fs::read(self.clip_dir(id).join(format!("{format_id}.bin"))).ok()
    }

    pub fn preview_path(&self, id: &str) -> Option<PathBuf> {
        let meta = self.get(id)?;
        let name = meta.preview_image.as_ref()?;
        let p = self.clip_dir(id).join(name);
        p.exists().then_some(p)
    }
}

/// Build a BMP file image from raw CF_DIB bytes (BITMAPINFOHEADER + pixels),
/// so the webview can render it as a data URL.
pub fn dib_to_bmp(dib: &[u8]) -> Option<Vec<u8>> {
    if dib.len() < 40 {
        return None;
    }
    let header_size = u32::from_le_bytes(dib[0..4].try_into().ok()?) as usize;
    let bit_count = u16::from_le_bytes(dib[14..16].try_into().ok()?) as u32;
    let compression = u32::from_le_bytes(dib[16..20].try_into().ok()?);
    let clr_used = u32::from_le_bytes(dib[32..36].try_into().ok()?);
    // Palette size: explicit count, or full palette for <=8bpp.
    let palette_entries = if clr_used != 0 {
        clr_used
    } else if bit_count <= 8 {
        1 << bit_count
    } else {
        0
    };
    // BI_BITFIELDS (3) stores 3 DWORD masks after the header (for BITMAPINFOHEADER).
    let masks = if compression == 3 && header_size == 40 { 12 } else { 0 };
    let pixel_offset = 14 + header_size + masks + (palette_entries as usize) * 4;
    let file_size = 14 + dib.len();
    let mut bmp = Vec::with_capacity(file_size);
    bmp.extend_from_slice(b"BM");
    bmp.extend_from_slice(&(file_size as u32).to_le_bytes());
    bmp.extend_from_slice(&0u32.to_le_bytes());
    bmp.extend_from_slice(&(pixel_offset as u32).to_le_bytes());
    bmp.extend_from_slice(dib);
    Some(bmp)
}

/// Extract file paths from CF_HDROP bytes (DROPFILES struct).
pub fn hdrop_paths(bytes: &[u8]) -> Vec<String> {
    if bytes.len() < 20 {
        return Vec::new();
    }
    let offset = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
    let wide = bytes[16] != 0;
    let mut paths = Vec::new();
    if wide {
        let data = &bytes[offset..];
        let units: Vec<u16> = data
            .chunks_exact(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        for part in units.split(|&u| u == 0) {
            if part.is_empty() {
                break;
            }
            paths.push(String::from_utf16_lossy(part));
        }
    } else {
        for part in bytes[offset..].split(|&b| b == 0) {
            if part.is_empty() {
                break;
            }
            paths.push(String::from_utf8_lossy(part).to_string());
        }
    }
    paths
}

pub fn file_name_of(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| path.to_string())
}
