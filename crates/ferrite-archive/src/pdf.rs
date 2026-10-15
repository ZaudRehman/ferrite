//! PDF load, search, and isolated render. Without `pdfium-render` the
//! fallback is metadata only and encrypted files fail closed.

use crate::error::{ArchiveError, ArchiveResult};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// True when the real pdfium engine is compiled in. The renderer child
/// reports this so the viewer can explain degraded mode honestly.
#[cfg(feature = "pdfium-render")]
pub const HAS_ENGINE: bool = true;
#[cfg(not(feature = "pdfium-render"))]
pub const HAS_ENGINE: bool = false;

/// Platform pdfium library file name, matching the pinned builds the
/// release workflow ships beside the binaries.
#[cfg(windows)]
pub const PDFIUM_LIB_NAME: &str = "pdfium.dll";
#[cfg(target_os = "macos")]
pub const PDFIUM_LIB_NAME: &str = "libpdfium.dylib";
#[cfg(all(not(windows), not(target_os = "macos")))]
pub const PDFIUM_LIB_NAME: &str = "libpdfium.so";

/// Renderer child binary name. Spawned as a sibling of the main exe (never
/// via PATH or CWD: blocks binary planting via download), so installers
/// must place it there.
#[cfg(windows)]
pub const RENDERER_EXE_NAME: &str = "ferrite-pdf-renderer.exe";
#[cfg(not(windows))]
pub const RENDERER_EXE_NAME: &str = "ferrite-pdf-renderer";

/// Directory holding the running exe. None when undeterminable (tests,
/// exotic hosts): callers treat that as "nothing shipped".
pub fn exe_sibling_dir() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// Boot-time PDF capability report. Filesystem only: binding here would init
/// the lib in-process, but the renderer child owns the single engine
/// instance by design. Returns (renderer_shipped, pdfium_source) with source
/// one of "env" | "sibling" | "system-or-missing" — system lookup is only
/// knowable by binding, so the label stays honest instead of claiming more.
pub fn pdf_boot_report() -> (bool, &'static str) {
    pdf_boot_report_for_dirs(
        exe_sibling_dir(),
        std::env::var("FERRITE_PDFIUM_PATH").ok().map(PathBuf::from),
    )
}

fn pdf_boot_report_for_dirs(
    exe_dir: Option<PathBuf>,
    env_dir: Option<PathBuf>,
) -> (bool, &'static str) {
    let renderer = exe_dir
        .as_ref()
        .map(|d| d.join(RENDERER_EXE_NAME).is_file())
        .unwrap_or(false);
    let pdfium = if env_dir.is_some() {
        "env"
    } else if exe_dir.as_ref().map(|d| dir_has_pdfium(d)).unwrap_or(false) {
        "sibling"
    } else {
        "system-or-missing"
    };
    (renderer, pdfium)
}

/// The shipped library file sits in this directory. Shared by the report
/// above and the bind path below so the two can never disagree on what
/// "shipped" means.
fn dir_has_pdfium(dir: &Path) -> bool {
    dir.join(PDFIUM_LIB_NAME).is_file()
}

/// Sibling pdfium directory when the shipped library file is present.
/// Pure shape (no bind): the bind itself stays in `PdfEngine::new`.
/// Feature-gated with its only caller (the engine bind path).
#[cfg(feature = "pdfium-render")]
fn sibling_pdfium_dir() -> Option<PathBuf> {
    exe_sibling_dir().filter(|d| dir_has_pdfium(d))
}

#[derive(Debug, Clone)]
pub struct PdfInfo {
    pub page_count: usize,
    pub title: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SearchHit {
    pub page_index: usize,
    pub bounds: Option<(f32, f32, f32, f32)>,
}

#[derive(Debug, Clone)]
pub struct FormField {
    pub name: String,
    pub field_type: String,
    pub value: Option<String>,
}

/// Fractional rectangle in page space, origin top-left (CSS-ready).
/// All values are clamped to 0.0..=1.0 by construction.
#[derive(Debug, Clone, Serialize)]
pub struct PageRect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

/// One selectable text line: visible text plus its fractional bounds.
#[derive(Debug, Clone, Serialize)]
pub struct PageLine {
    pub text: String,
    pub rect: PageRect,
}

/// One clickable link region. `page` is an internal destination page
/// index; `url` is an external http(s) URL (anything else is dropped
/// at construction, fail-closed). At most one is set.
#[derive(Debug, Clone, Serialize)]
pub struct PageLink {
    pub rect: PageRect,
    pub page: Option<u32>,
    pub url: Option<String>,
}

/// One outline (table-of-contents) entry.
#[derive(Debug, Clone, Serialize)]
pub struct TocEntry {
    pub title: String,
    pub page: u32,
    pub depth: u8,
}

/// One form field, stable-indexed in annotation order.
#[derive(Debug, Clone, Serialize)]
pub struct FieldState {
    pub index: usize,
    pub name: String,
    pub kind: String,
    pub value: Option<String>,
    pub options: Vec<String>,
    pub checked: bool,
    pub editable: bool,
}

/// Document permission summary (fail-open display only: the viewer
/// enforces these, the engine does not).
#[derive(Debug, Clone, Serialize)]
pub struct DocPermissions {
    pub print: bool,
    pub copy: bool,
    pub fill: bool,
}

/// Document metadata.
#[derive(Debug, Clone, Serialize)]
pub struct DocMeta {
    pub title: Option<String>,
    pub author: Option<String>,
    pub subject: Option<String>,
    pub keywords: Option<String>,
    pub pages: usize,
}

#[cfg(feature = "pdfium-render")]
mod inner {
    use super::*;
    use pdfium_render::prelude::*;
    use std::sync::Arc;

    /// Bounds, shared by every overlay producer.
    const MAX_SEARCH_HITS_PER_PAGE: usize = 500;
    const MAX_LINE_CHARS: usize = 20_000;
    const MAX_LINES_PER_PAGE: usize = 3_000;
    const MAX_OUTLINE_ENTRIES: usize = 512;
    const MAX_OUTLINE_DEPTH: u8 = 8;
    const MAX_FIELDS: usize = 2_048;
    const MAX_SEARCH_PAGES: usize = 1_000;
    const MAX_URL_CHARS: usize = 2_048;

    pub struct PdfEngine {
        lib: Arc<Pdfium>,
    }

    impl PdfEngine {
        /// Bind the library. Succeeds once per process: pdfium bindings
        /// initialize exactly once (`PdfiumLibraryBindingsAlreadyInitialized`
        /// after that), so production holds a single engine per process:
        /// the renderer child owns its instance, and the shell talks to
        /// the child instead of binding in-process.
        ///
        /// Resolution order: explicit `FERRITE_PDFIUM_PATH` first (operator
        /// intent: failure here errors out, never silently degrades), then
        /// the shipped sibling library beside the exe (present-but-unbindable
        /// warns and falls through to system), then the system library.
        /// CWD is never searched. Blocks DLL planting via download.
        pub fn new() -> ArchiveResult<Self> {
            // CWD is never searched. Blocks DLL planting via download.
            let lib = if let Ok(path) = std::env::var("FERRITE_PDFIUM_PATH") {
                Pdfium::new(
                    Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&path))
                        .map_err(|e| ArchiveError::Pdf(e.to_string()))?,
                )
            } else if let Some(dir) = super::sibling_pdfium_dir() {
                match Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&dir)) {
                    Ok(bindings) => Pdfium::new(bindings),
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "bundled pdfium beside the exe failed to bind; trying system library"
                        );
                        Pdfium::new(
                            Pdfium::bind_to_system_library()
                                .map_err(|e| ArchiveError::Pdf(e.to_string()))?,
                        )
                    }
                }
            } else {
                Pdfium::new(
                    Pdfium::bind_to_system_library()
                        .map_err(|e| ArchiveError::Pdf(e.to_string()))?,
                )
            };
            Ok(Self { lib: Arc::new(lib) })
        }

        pub fn load(&self, bytes: &[u8], password: Option<&str>) -> ArchiveResult<PdfDocument> {
            Ok(PdfDocument {
                lib: Arc::clone(&self.lib),
                bytes: bytes.to_vec(),
                password: password.map(|s| zeroize::Zeroizing::new(s.to_owned())),
                dirty: false,
            })
        }
    }

    pub struct PdfDocument {
        lib: Arc<Pdfium>,
        bytes: Vec<u8>,
        password: Option<zeroize::Zeroizing<String>>,
        dirty: bool,
    }

    impl PdfDocument {
        fn password_str(&self) -> Option<&str> {
            self.password.as_deref().map(|s| s.as_str())
        }

        fn with_doc<F, R>(&self, f: F) -> ArchiveResult<R>
        where
            F: FnOnce(&PdfiumDocument) -> ArchiveResult<R>,
        {
            let doc = self
                .lib
                .load_pdf_from_byte_slice(&self.bytes, self.password_str())
                .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
            f(&doc)
        }

        fn with_doc_mut<F, R>(&mut self, f: F) -> ArchiveResult<R>
        where
            F: FnOnce(&mut PdfiumDocument) -> ArchiveResult<R>,
        {
            // Clone-then-replace: the loaded document borrows the source
            // slice, so mutations serialize into a fresh buffer that
            // replaces the live bytes only after the closure succeeds.
            // (A failed fill leaves the previous bytes untouched.)
            let src = self.bytes.clone();
            let mut doc = self
                .lib
                .load_pdf_from_byte_slice(&src, self.password_str())
                .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
            let out = f(&mut doc)?;
            // Mutations must land in the live bytes: without this write-
            // back, fills apply to a dropped document and save_bytes
            // serializes the pristine original (silent data loss with a
            // dirty flag claiming otherwise).
            self.bytes = doc
                .save_to_bytes()
                .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
            Ok(out)
        }

        /// Points-space quad to CSS-ready fractions (origin top-left).
        fn frac(q: &PdfQuadPoints, page_w: f32, page_h: f32) -> Option<PageRect> {
            if page_w <= 0.0 || page_h <= 0.0 {
                return None;
            }
            let l = (q.left().value / page_w).clamp(0.0, 1.0);
            let r = (q.right().value / page_w).clamp(0.0, 1.0);
            // PDF y grows upward; CSS y grows downward.
            let t = ((page_h - q.top().value) / page_h).clamp(0.0, 1.0);
            let b = ((page_h - q.bottom().value) / page_h).clamp(0.0, 1.0);
            if r <= l || b <= t {
                return None;
            }
            Some(PageRect {
                x: l,
                y: t,
                w: r - l,
                h: b - t,
            })
        }

        /// External URLs only: http(s), bounded, no control characters.
        /// Everything else (javascript:, file:, data:) is dropped.
        fn clean_url(raw: &str) -> Option<String> {
            let s = raw.trim();
            if s.len() > MAX_URL_CHARS || s.is_empty() {
                return None;
            }
            if !(s.starts_with("http://") || s.starts_with("https://")) {
                return None;
            }
            if s.chars().any(char::is_control) {
                return None;
            }
            Some(s.to_owned())
        }

        pub fn info(&self) -> PdfInfo {
            self.with_doc(|doc| {
                Ok(PdfInfo {
                    page_count: doc.pages().len() as usize,
                    title: doc
                        .metadata()
                        .get(pdfium_render::prelude::PdfDocumentMetadataTagType::Title)
                        .map(|t| t.value().to_string()),
                })
            })
            .unwrap_or(PdfInfo {
                page_count: 0,
                title: None,
            })
        }

        pub fn render_page(&self, page_index: usize, scale: f32) -> ArchiveResult<Vec<u8>> {
            let (w, h) = self.page_dimensions(page_index).unwrap_or((800.0, 1100.0));
            self.with_doc(|doc| {
                let page = doc
                    .pages()
                    .get(page_index as i32)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let bitmap = page
                    .render_with_config(
                        &PdfRenderConfig::new()
                            .set_target_width((w * scale) as i32)
                            .set_target_height((h * scale) as i32),
                    )
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                Ok(bitmap.as_raw_bytes().to_vec())
            })
        }

        fn page_dimensions(&self, page_index: usize) -> Option<(f32, f32)> {
            self.with_doc(|doc| {
                let page = match doc.pages().get(page_index as i32) {
                    Ok(p) => p,
                    Err(_) => return Ok(None),
                };
                Ok(Some((page.width().value, page.height().value)))
            })
            .ok()
            .flatten()
        }

        /// Page size in points. `None` when the page is unreadable;
        /// callers fall back to US-Letter ratio, never to zeros.
        pub fn page_size(&self, page_index: usize) -> Option<(f32, f32)> {
            self.page_dimensions(page_index)
        }

        pub fn search(&self, query: &str) -> Vec<SearchHit> {
            self.with_doc(|doc| {
                let mut out = Vec::new();
                for (i, page) in doc.pages().iter().enumerate() {
                    if let Ok(text) = page.text() {
                        if text.to_string().contains(query) {
                            out.push(SearchHit {
                                page_index: i,
                                bounds: None,
                            });
                        }
                    }
                    if out.len() > MAX_SEARCH_PAGES {
                        break;
                    }
                }
                Ok(out)
            })
            .unwrap_or_default()
        }

        /// Bounded search on one page: match rectangles as fractions.
        pub fn search_page(&self, page_index: usize, query: &str, case: bool) -> Vec<PageRect> {
            if query.is_empty() || query.len() > 256 {
                return Vec::new();
            }
            self.with_doc(|doc| {
                let page = doc
                    .pages()
                    .get(page_index as i32)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let (pw, ph) = match self.page_dimensions(page_index) {
                    Some(d) => d,
                    None => return Ok(Vec::new()),
                };
                let text = page.text().map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let options = PdfSearchOptions::new().match_case(case);
                let search = text
                    .search(query, &options)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let mut out = Vec::new();
                for segments in search.iter(PdfSearchDirection::SearchForward) {
                    for segment in segments.iter() {
                        if let Some(r) = Self::frac(&segment.bounds().to_quad_points(), pw, ph) {
                            out.push(r);
                        }
                        if out.len() >= MAX_SEARCH_HITS_PER_PAGE {
                            return Ok(out);
                        }
                    }
                }
                Ok(out)
            })
            .unwrap_or_default()
        }

        /// Selectable text layer: characters grouped into lines, fractions.
        /// Empty when the page has no extractable text (scanned pages fail
        /// open as image-only: the viewer shows the bitmap, no layer).
        pub fn page_lines(&self, page_index: usize) -> Vec<PageLine> {
            self.with_doc(|doc| {
                let page = doc
                    .pages()
                    .get(page_index as i32)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let (pw, ph) = match self.page_dimensions(page_index) {
                    Some(d) => d,
                    None => return Ok(Vec::new()),
                };
                let text = page.text().map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let chars = text.chars();
                // (top, left, right, bottom, char) in points.
                let mut raw: Vec<(f32, f32, f32, f32, char)> = Vec::new();
                let n = chars.len().min(MAX_LINE_CHARS);
                for i in 0..n {
                    let Ok(ch) = chars.get(i) else {
                        continue;
                    };
                    let Some(c) = ch.unicode_char() else {
                        continue;
                    };
                    if c.is_control() {
                        continue;
                    }
                    let Ok(q) = ch.tight_bounds().map(|r| r.to_quad_points()) else {
                        continue;
                    };
                    raw.push((
                        q.top().value,
                        q.left().value,
                        q.right().value,
                        q.bottom().value,
                        c,
                    ));
                }
                // Rows: same baseline within half a line height.
                raw.sort_by(|a, b| {
                    b.0.partial_cmp(&a.0)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
                });
                // Row tuples are (top, left, right, bottom, char).
                type RawRow = Vec<(f32, f32, f32, f32, char)>;
                let mut rows: Vec<RawRow> = Vec::new();
                for g in raw {
                    let placed = rows.iter_mut().find(|row| {
                        let (rt, _, _, rb, _) = row[0];
                        let h = (rt - rb).max(1.0);
                        (g.0 - rt).abs() <= (h * 0.5).max(1.5)
                    });
                    match placed {
                        Some(row) => row.push(g),
                        None => {
                            if rows.len() >= MAX_LINES_PER_PAGE {
                                break;
                            }
                            rows.push(vec![g]);
                        }
                    }
                }
                let mut out = Vec::with_capacity(rows.len());
                for mut row in rows {
                    row.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
                    let l = row.iter().map(|g| g.1).fold(f32::INFINITY, f32::min);
                    let r = row.iter().map(|g| g.2).fold(f32::NEG_INFINITY, f32::max);
                    let t = row.iter().map(|g| g.0).fold(f32::NEG_INFINITY, f32::max);
                    let b = row.iter().map(|g| g.3).fold(f32::INFINITY, f32::min);
                    let text: String = row.iter().map(|g| g.4).collect();
                    if text.trim().is_empty() {
                        continue;
                    }
                    let x = (l / pw).clamp(0.0, 1.0);
                    let w = ((r - l) / pw).clamp(0.0, 1.0);
                    let y = ((ph - t) / ph).clamp(0.0, 1.0);
                    let h = ((t - b) / ph).clamp(0.0, 1.0);
                    if w > 0.0 && h > 0.0 {
                        out.push(PageLine {
                            text,
                            rect: PageRect { x, y, w, h },
                        });
                    }
                }
                Ok(out)
            })
            .unwrap_or_default()
        }

        /// Clickable regions: internal destinations win over URI actions;
        /// external URLs pass the fail-closed filter, the rest is dropped.
        pub fn page_links(&self, page_index: usize) -> Vec<PageLink> {
            self.with_doc(|doc| {
                let page = doc
                    .pages()
                    .get(page_index as i32)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let (pw, ph) = match self.page_dimensions(page_index) {
                    Some(d) => d,
                    None => return Ok(Vec::new()),
                };
                let mut out = Vec::new();
                for link in page.links().iter() {
                    let Ok(rect) = link.rect() else {
                        continue;
                    };
                    let Some(r) = Self::frac(&rect.to_quad_points(), pw, ph) else {
                        continue;
                    };
                    let mut entry = PageLink {
                        rect: r,
                        page: None,
                        url: None,
                    };
                    if let Some(dest) = link.destination() {
                        if let Ok(idx) = dest.page_index() {
                            entry.page = Some(idx as u32);
                        }
                    }
                    if entry.page.is_none() {
                        if let Some(action) = link.action() {
                            if let Some(local) = action.as_local_destination_action() {
                                if let Ok(dest) = local.destination() {
                                    if let Ok(idx) = dest.page_index() {
                                        entry.page = Some(idx as u32);
                                    }
                                }
                            } else if let Some(uri) = action.as_uri_action() {
                                if let Ok(u) = uri.uri() {
                                    entry.url = Self::clean_url(&u);
                                }
                            }
                            // Launch/remote/embedded/unsupported actions are
                            // dropped: executing files is never a viewer job.
                        }
                    }
                    if entry.page.is_some() || entry.url.is_some() {
                        out.push(entry);
                    }
                    if out.len() >= 1_000 {
                        break;
                    }
                }
                Ok(out)
            })
            .unwrap_or_default()
        }

        /// Outline walk: title plus destination page, depth-capped.
        pub fn outline(&self) -> Vec<TocEntry> {
            self.with_doc(|doc| {
                let mut out = Vec::new();
                // Pre-order walk, document order preserved: siblings are
                // pushed reversed so the stack pops them front-first.
                let mut stack = Vec::new();
                if let Some(root) = doc.bookmarks().root() {
                    let mut kids = Vec::new();
                    let mut next = root.first_child();
                    while let Some(bm) = next {
                        next = bm.next_sibling();
                        kids.push(bm);
                    }
                    for bm in kids.into_iter().rev() {
                        stack.push((bm, 0u8));
                    }
                }
                while let Some((bm, depth)) = stack.pop() {
                    if out.len() >= MAX_OUTLINE_ENTRIES {
                        break;
                    }
                    let mut page = None;
                    if let Some(dest) = bm.destination() {
                        page = dest.page_index().ok().map(|i| i as u32);
                    }
                    if page.is_none() {
                        if let Some(action) = bm.action() {
                            if let Some(local) = action.as_local_destination_action() {
                                if let Ok(dest) = local.destination() {
                                    page = dest.page_index().ok().map(|i| i as u32);
                                }
                            }
                        }
                    }
                    if let (Some(title), Some(page)) = (bm.title(), page) {
                        let title = title.trim().to_owned();
                        if !title.is_empty() && title.len() <= 512 {
                            out.push(TocEntry { title, page, depth });
                        }
                    }
                    if depth < MAX_OUTLINE_DEPTH {
                        let mut kids = Vec::new();
                        let mut next = bm.first_child();
                        while let Some(k) = next {
                            next = k.next_sibling();
                            kids.push(k);
                        }
                        for k in kids.into_iter().rev() {
                            stack.push((k, depth + 1));
                        }
                    }
                }
                // The stack pops front-first, so `out` is already in
                // document order: parents before children, siblings in order.
                Ok(out)
            })
            .unwrap_or_default()
        }

        /// Legacy flat field list (name/type/value). Backed by the same
        /// annotation walk as [`PdfDocument::form_state`].
        pub fn form_fields(&self) -> Vec<FormField> {
            self.form_state()
                .into_iter()
                .map(|f| FormField {
                    name: f.name,
                    field_type: f.kind,
                    value: f.value,
                })
                .collect()
        }

        /// Full form state in stable annotation order. Widget bounds are
        /// not exposed by the engine API, so fields render in the viewer
        /// side panel (indexed, never positional): complete and honest,
        /// no overlay guessing.
        pub fn form_state(&self) -> Vec<FieldState> {
            self.with_doc(|doc| {
                let fillable = self.permissions_summary().map(|p| p.fill).unwrap_or(false);
                let mut out = Vec::new();
                let pages = doc.pages();
                for i in 0..pages.len() {
                    let Ok(page) = pages.get(i) else {
                        continue;
                    };
                    for annot in page.annotations().iter() {
                        let Some(field) = annot.as_form_field() else {
                            continue;
                        };
                        let name = field.name().unwrap_or_default();
                        if name.is_empty() || name.len() > 256 {
                            continue;
                        }
                        let (kind, value, options, checked, supported) = match field {
                            PdfFormField::Text(f) => {
                                ("text".to_owned(), f.value(), Vec::new(), false, true)
                            }
                            PdfFormField::Checkbox(f) => (
                                "checkbox".to_owned(),
                                None,
                                Vec::new(),
                                f.is_checked().unwrap_or(false),
                                true,
                            ),
                            PdfFormField::RadioButton(f) => (
                                "radio".to_owned(),
                                None,
                                Vec::new(),
                                f.is_checked().unwrap_or(false),
                                true,
                            ),
                            PdfFormField::ComboBox(f) => (
                                "combo".to_owned(),
                                f.value(),
                                f.options()
                                    .iter()
                                    .filter_map(|o| o.label().cloned())
                                    .take(256)
                                    .collect(),
                                false,
                                false,
                            ),
                            PdfFormField::ListBox(f) => (
                                "list".to_owned(),
                                f.value(),
                                f.options()
                                    .iter()
                                    .filter_map(|o| o.label().cloned())
                                    .take(256)
                                    .collect(),
                                false,
                                false,
                            ),
                            _ => ("unknown".to_owned(), None, Vec::new(), false, false),
                        };
                        out.push(FieldState {
                            index: out.len(),
                            name,
                            kind,
                            value,
                            options,
                            checked,
                            editable: fillable && supported,
                        });
                        if out.len() >= MAX_FIELDS {
                            return Ok(out);
                        }
                    }
                }
                Ok(out)
            })
            .unwrap_or_default()
        }

        fn field_at_mut(
            &mut self,
            index: usize,
        ) -> ArchiveResult<(usize, usize, PdfFormFieldType)> {
            // Locate the (page, annotation) pair for a stable field index
            // without holding borrows across the mutation.
            let locator = self.with_doc(|doc| {
                let pages = doc.pages();
                let mut seen = 0usize;
                for i in 0..pages.len() {
                    let Ok(page) = pages.get(i) else {
                        continue;
                    };
                    for (a, annot) in page.annotations().iter().enumerate() {
                        if annot.as_form_field().is_none() {
                            continue;
                        }
                        if seen == index {
                            let kind = annot
                                .as_form_field()
                                .map(|f| f.field_type())
                                .unwrap_or(PdfFormFieldType::Unknown);
                            return Ok((i as usize, a, kind));
                        }
                        seen += 1;
                    }
                }
                Err(ArchiveError::Pdf(
                    "form field index out of range".to_string(),
                ))
            })?;
            Ok(locator)
        }

        /// Fill a text field by stable index. No-op positions (combo/list
        /// have no engine value setter) fail closed with a named error.
        pub fn fill_text(&mut self, index: usize, value: &str) -> ArchiveResult<()> {
            if value.len() > 16_384 {
                return Err(ArchiveError::Pdf("form value too long".to_string()));
            }
            let (page_idx, annot_idx, kind) = self.field_at_mut(index)?;
            if kind != PdfFormFieldType::Text {
                return Err(ArchiveError::Pdf("field is not fillable text".to_string()));
            }
            self.with_doc_mut(|doc| {
                let mut page = doc
                    .pages_mut()
                    .get(page_idx as i32)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let mut annot = page
                    .annotations_mut()
                    .get(annot_idx)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let field = annot
                    .as_form_field_mut()
                    .ok_or_else(|| ArchiveError::Pdf("field vanished".to_string()))?;
                let Some(text) = field.as_text_field_mut() else {
                    return Err(ArchiveError::Pdf("field is not fillable text".to_string()));
                };
                text.set_value(value)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                Ok(())
            })?;
            self.dirty = true;
            Ok(())
        }

        pub fn fill_check(&mut self, index: usize, checked: bool) -> ArchiveResult<()> {
            let (page_idx, annot_idx, kind) = self.field_at_mut(index)?;
            if kind != PdfFormFieldType::Checkbox {
                return Err(ArchiveError::Pdf("field is not a checkbox".to_string()));
            }
            self.with_doc_mut(|doc| {
                let mut page = doc
                    .pages_mut()
                    .get(page_idx as i32)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let mut annot = page
                    .annotations_mut()
                    .get(annot_idx)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let field = annot
                    .as_form_field_mut()
                    .ok_or_else(|| ArchiveError::Pdf("field vanished".to_string()))?;
                let Some(cb) = field.as_checkbox_field_mut() else {
                    return Err(ArchiveError::Pdf("field is not a checkbox".to_string()));
                };
                cb.set_checked(checked)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                Ok(())
            })?;
            self.dirty = true;
            Ok(())
        }

        pub fn fill_radio(&mut self, index: usize) -> ArchiveResult<()> {
            let (page_idx, annot_idx, kind) = self.field_at_mut(index)?;
            if kind != PdfFormFieldType::RadioButton {
                return Err(ArchiveError::Pdf("field is not a radio button".to_string()));
            }
            self.with_doc_mut(|doc| {
                let mut page = doc
                    .pages_mut()
                    .get(page_idx as i32)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let mut annot = page
                    .annotations_mut()
                    .get(annot_idx)
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                let field = annot
                    .as_form_field_mut()
                    .ok_or_else(|| ArchiveError::Pdf("field vanished".to_string()))?;
                let Some(rb) = field.as_radio_button_field_mut() else {
                    return Err(ArchiveError::Pdf("field is not a radio button".to_string()));
                };
                rb.set_checked()
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))?;
                Ok(())
            })?;
            self.dirty = true;
            Ok(())
        }

        /// Whether fills were applied since load. The viewer enables
        /// "Save a copy" on this, never on guesswork.
        pub fn is_dirty(&self) -> bool {
            self.dirty
        }

        /// Serialize the (possibly filled) document. Bytes stay in memory;
        /// the shell decides whether the user asked to write them anywhere.
        pub fn save_bytes(&self) -> ArchiveResult<Vec<u8>> {
            self.with_doc(|doc| {
                doc.save_to_bytes()
                    .map_err(|e| ArchiveError::Pdf(e.to_string()))
            })
        }

        /// Engine-reported permissions. Unencrypted documents report
        /// all-true; the viewer enforces, the engine does not.
        pub fn permissions_summary(&self) -> ArchiveResult<DocPermissions> {
            self.with_doc(|doc| {
                let p = doc.permissions();
                let print = p.can_print_high_quality().unwrap_or(false)
                    || p.can_print_only_low_quality().unwrap_or(false);
                Ok(DocPermissions {
                    print,
                    copy: p.can_extract_text_and_graphics().unwrap_or(false),
                    fill: p
                        .can_fill_existing_interactive_form_fields()
                        .unwrap_or(false),
                })
            })
        }

        pub fn metadata_full(&self) -> DocMeta {
            let pages = self.info().page_count;
            self.with_doc(|doc| {
                let meta = doc.metadata();
                let tag = |t| {
                    meta.get(t)
                        .map(|v| v.value().to_string())
                        .filter(|s| !s.trim().is_empty() && s.len() <= 512)
                };
                Ok(DocMeta {
                    title: tag(PdfDocumentMetadataTagType::Title),
                    author: tag(PdfDocumentMetadataTagType::Author),
                    subject: tag(PdfDocumentMetadataTagType::Subject),
                    keywords: tag(PdfDocumentMetadataTagType::Keywords),
                    pages,
                })
            })
            .unwrap_or(DocMeta {
                title: None,
                author: None,
                subject: None,
                keywords: None,
                pages,
            })
        }

        pub fn render_all_pages(&self, scale: f32) -> ArchiveResult<Vec<Vec<u8>>> {
            let info = self.info();
            let mut out = Vec::with_capacity(info.page_count);
            for i in 0..info.page_count {
                out.push(self.render_page(i, scale)?);
            }
            Ok(out)
        }

        pub fn wgpu_texture_data(
            &self,
            page_index: usize,
            scale: f32,
        ) -> ArchiveResult<(u32, u32, Vec<u8>)> {
            // Single render; dimensions from the page (fallback US-Letter
            // ratio when the page is unreadable).
            let (w, h) = self.page_dimensions(page_index).unwrap_or((800.0, 1100.0));
            let rgba = self.render_page(page_index, scale)?;
            Ok(((w * scale) as u32, (h * scale) as u32, rgba))
        }

        pub fn needs_password(bytes: &[u8]) -> bool {
            bytes.windows(8).any(|w| w == b"/Encrypt")
        }
    }
    type PdfiumDocument<'a> = pdfium_render::prelude::PdfDocument<'a>;
}

#[cfg(not(feature = "pdfium-render"))]
mod inner {
    use super::*;

    pub struct PdfEngine;

    impl PdfEngine {
        pub fn new() -> ArchiveResult<Self> {
            Ok(Self)
        }

        pub fn load(&self, bytes: &[u8], _password: Option<&str>) -> ArchiveResult<PdfDocument> {
            if bytes.is_empty() {
                return Err(ArchiveError::Pdf("empty pdf".to_string()));
            }
            // No engine to verify against, so encrypted files stay closed.
            if bytes.windows(8).any(|w| w == b"/Encrypt") {
                return Err(ArchiveError::Pdf(
                    "encrypted pdf requires the pdfium-render feature".to_string(),
                ));
            }
            Ok(PdfDocument {
                page_count: 1,
                data: bytes.to_vec(),
            })
        }
    }

    pub struct PdfDocument {
        page_count: usize,
        data: Vec<u8>,
    }

    impl PdfDocument {
        pub fn info(&self) -> PdfInfo {
            PdfInfo {
                page_count: self.page_count,
                title: None,
            }
        }

        pub fn page_size(&self, _page_index: usize) -> Option<(f32, f32)> {
            // No engine to measure with; the viewer treats unknown size
            // as "render unavailable", never as zeros.
            None
        }

        pub fn render_page(&self, _page_index: usize, _scale: f32) -> ArchiveResult<Vec<u8>> {
            // Fail-closed placeholder: without the pdfium engine there is no
            // rasterizer, and a white pixel would masquerade as a blank page.
            // Callers surface this as an explicit "needs pdfium-render" error.
            Err(ArchiveError::Pdf(
                "pdf rendering needs the pdfium-render feature (see docs/SETUP.md)".to_string(),
            ))
        }

        pub fn search(&self, query: &str) -> Vec<SearchHit> {
            let text = String::from_utf8_lossy(&self.data);
            if text.contains(query) {
                vec![SearchHit {
                    page_index: 0,
                    bounds: None,
                }]
            } else {
                Vec::new()
            }
        }

        pub fn search_page(&self, _page_index: usize, _query: &str, _case: bool) -> Vec<PageRect> {
            // No text objects without the engine: the raw-byte search
            // above is the whole story here.
            Vec::new()
        }

        pub fn page_lines(&self, _page_index: usize) -> Vec<PageLine> {
            Vec::new()
        }

        pub fn page_links(&self, _page_index: usize) -> Vec<PageLink> {
            Vec::new()
        }

        pub fn outline(&self) -> Vec<TocEntry> {
            Vec::new()
        }

        pub fn form_fields(&self) -> Vec<FormField> {
            self.form_state()
                .into_iter()
                .map(|f| FormField {
                    name: f.name,
                    field_type: f.kind,
                    value: f.value,
                })
                .collect()
        }

        pub fn form_state(&self) -> Vec<FieldState> {
            let text = String::from_utf8_lossy(&self.data);
            let mut fields = Vec::new();
            let mut start = 0;
            while let Some(pos) = text[start..].find("/T (") {
                let abs = start + pos + 4;
                if let Some(end) = text[abs..].find(')') {
                    let name = text[abs..abs + end].to_owned();
                    let slice = &text[start..abs + end];
                    let field_type = if slice.contains("/FT /Tx") {
                        "text"
                    } else if slice.contains("/FT /Btn") {
                        "button"
                    } else if slice.contains("/FT /Ch") {
                        "choice"
                    } else {
                        "unknown"
                    }
                    .to_owned();
                    let value = slice.find("/V (").and_then(|p| {
                        let s = &slice[p + 4..];
                        s.find(')').map(|e| s[..e].to_owned())
                    });
                    let index = fields.len();
                    fields.push(FieldState {
                        index,
                        name,
                        kind: field_type,
                        value,
                        options: Vec::new(),
                        checked: false,
                        editable: false,
                    });
                    start = abs + end + 1;
                } else {
                    break;
                }
                if fields.len() > 128 {
                    break;
                }
            }
            fields
        }

        pub fn fill_text(&mut self, _index: usize, _value: &str) -> ArchiveResult<()> {
            Err(ArchiveError::Pdf(
                "form fill needs the pdfium-render feature (see docs/SETUP.md)".to_string(),
            ))
        }

        pub fn fill_check(&mut self, _index: usize, _checked: bool) -> ArchiveResult<()> {
            Err(ArchiveError::Pdf(
                "form fill needs the pdfium-render feature (see docs/SETUP.md)".to_string(),
            ))
        }

        pub fn fill_radio(&mut self, _index: usize) -> ArchiveResult<()> {
            Err(ArchiveError::Pdf(
                "form fill needs the pdfium-render feature (see docs/SETUP.md)".to_string(),
            ))
        }

        pub fn is_dirty(&self) -> bool {
            false
        }

        /// Byte passthrough: saving an unmodified copy needs no engine.
        pub fn save_bytes(&self) -> ArchiveResult<Vec<u8>> {
            Ok(self.data.clone())
        }

        /// Unknown without the engine: allow viewing, forbid filling.
        pub fn permissions_summary(&self) -> ArchiveResult<DocPermissions> {
            Ok(DocPermissions {
                print: true,
                copy: true,
                fill: false,
            })
        }

        pub fn metadata_full(&self) -> DocMeta {
            DocMeta {
                title: None,
                author: None,
                subject: None,
                keywords: None,
                pages: self.page_count,
            }
        }

        pub fn render_all_pages(&self, scale: f32) -> ArchiveResult<Vec<Vec<u8>>> {
            let mut out = Vec::new();
            for i in 0..self.page_count {
                out.push(self.render_page(i, scale)?);
            }
            Ok(out)
        }

        pub fn wgpu_texture_data(
            &self,
            page_index: usize,
            scale: f32,
        ) -> ArchiveResult<(u32, u32, Vec<u8>)> {
            let rgba = self.render_page(page_index, scale)?;
            debug_assert_eq!(rgba.len(), 4);
            Ok((1, 1, rgba))
        }
    }
}

pub use inner::{PdfDocument, PdfEngine};

#[derive(Debug)]
pub struct IsolatedPdfRenderer {
    child: Option<tokio::process::Child>,
    shm: Option<crate::shm::ShmHandle>,
}

impl IsolatedPdfRenderer {
    /// Caps: 64 MB input, page max 10000, password max 1024 bytes.
    pub const MAX_PDF_BYTES: usize = 64 * 1024 * 1024;
    pub const MAX_PAGE: usize = 10_000;

    pub fn spawn() -> ArchiveResult<Self> {
        let mut out = Self {
            child: None,
            shm: None,
        };
        out.ensure_spawned()?;
        Ok(out)
    }

    /// Spawn (or respawn) the child. The child serves many renders over
    /// one stdin/stdout pair until EOF; a dead child is respawned here
    /// so one crash costs a page, never the session.
    pub fn ensure_spawned(&mut self) -> ArchiveResult<()> {
        if self.is_alive() {
            return Ok(());
        }
        // Single source of truth with the boot report and the installers:
        // the child lives beside the main exe under one shared name.
        let path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(RENDERER_EXE_NAME)))
            .unwrap_or_else(|| RENDERER_EXE_NAME.into());
        let child = tokio::process::Command::new(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|e| {
                tracing::warn!(%e, path = %path.display(), "pdf renderer spawn failed");
                ArchiveError::Pdf("pdf renderer unavailable (spawn failed)".to_string())
            })?;
        self.child = Some(child);
        Ok(())
    }

    /// Kill the child now. Sessions call this on close/evict; dropping
    /// the handle does not kill it (tokio default), so "just drop it"
    /// would orphan renderer processes.
    pub fn shutdown(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.start_kill();
        }
        self.child = None;
        self.shm = None;
    }

    /// Page renders get a tight budget; metadata ops (search across a
    /// thousand pages, outline walks) get a roomier one. Both bounded:
    /// a wedged child is killed, never waited out.
    const PAGE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);
    const META_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(60);
    const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
    const MAX_SAVE_BYTES: usize = 128 * 1024 * 1024;

    /// One length-prefixed round trip. `file_path` switches the payload
    /// channel: page bitmaps travel via the shm file (pipes stall past
    /// buffer size), everything else comes back on stdout.
    async fn transact(
        &mut self,
        header: &str,
        payloads: &[&[u8]],
        file_path: Option<&std::path::Path>,
        timeout: std::time::Duration,
    ) -> ArchiveResult<Vec<u8>> {
        let child = match &mut self.child {
            Some(c) => c,
            None => {
                return Err(ArchiveError::Pdf(
                    "pdf renderer unavailable (spawn failed)".to_string(),
                ));
            }
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if let Some(stdin) = child.stdin.as_mut() {
            let mut failed = false;
            if stdin.write_all(header.as_bytes()).await.is_err() {
                failed = true;
            }
            for p in payloads {
                if stdin.write_all(p).await.is_err() {
                    failed = true;
                    break;
                }
            }
            if !failed && stdin.flush().await.is_err() {
                failed = true;
            }
            if failed {
                self.shutdown();
                return Err(ArchiveError::Pdf("pdf renderer stdin failed".to_string()));
            }
        } else {
            self.shutdown();
            return Err(ArchiveError::Pdf("pdf renderer stdin failed".to_string()));
        }
        let mut len_buf = [0u8; 4];
        let stale = match &mut self.child {
            Some(child) => match child.stdout.as_mut() {
                Some(stdout) => tokio::time::timeout(timeout, stdout.read_exact(&mut len_buf))
                    .await
                    .is_err(),
                None => true,
            },
            None => true,
        };
        if stale {
            self.shutdown();
            return Err(ArchiveError::Pdf("pdf renderer timed out".to_string()));
        }
        let len = u32::from_le_bytes(len_buf) as usize;
        if let Some(path) = file_path {
            if len == 0 || len > 50 * 1024 * 1024 {
                return Err(ArchiveError::Pdf(
                    "pdf renderer produced no output".to_string(),
                ));
            }
            let data = std::fs::read(path).map_err(ArchiveError::Io)?;
            if data.len() != len {
                return Err(ArchiveError::Pdf(
                    "pdf renderer bitmap truncated".to_string(),
                ));
            }
            Ok(data)
        } else {
            if len > Self::MAX_JSON_BYTES {
                self.shutdown();
                return Err(ArchiveError::Pdf(
                    "pdf renderer response too large".to_string(),
                ));
            }
            if len == 0 {
                return Err(ArchiveError::Pdf(
                    "pdf renderer produced no output".to_string(),
                ));
            }
            let child = match &mut self.child {
                Some(c) => c,
                None => {
                    return Err(ArchiveError::Pdf("pdf renderer timed out".to_string()));
                }
            };
            let Some(stdout) = child.stdout.as_mut() else {
                self.shutdown();
                return Err(ArchiveError::Pdf("pdf renderer timed out".to_string()));
            };
            let mut out = vec![0u8; len];
            if tokio::time::timeout(timeout, stdout.read_exact(&mut out))
                .await
                .is_err()
            {
                self.shutdown();
                return Err(ArchiveError::Pdf("pdf renderer timed out".to_string()));
            }
            Ok(out)
        }
    }

    fn parse_json(raw: Vec<u8>) -> ArchiveResult<serde_json::Value> {
        serde_json::from_slice(&raw)
            .map_err(|_| ArchiveError::Pdf("pdf renderer sent bad json".to_string()))
    }

    /// Load a document into the child session. Returns (pages, locked):
    /// `locked` without an error means "needs a password".
    pub async fn open_document(
        &mut self,
        bytes: &[u8],
        password: Option<&str>,
    ) -> ArchiveResult<(usize, bool)> {
        if bytes.is_empty() || bytes.len() > Self::MAX_PDF_BYTES {
            return Err(ArchiveError::Pdf("pdf length out of bounds".to_string()));
        }
        let pwd_len = password.map(|p| p.len()).unwrap_or(0);
        if pwd_len > 1024 {
            return Err(ArchiveError::Pdf("password too long".to_string()));
        }
        self.ensure_spawned()?;
        let header = format!("OPEN {} {}\n", bytes.len(), pwd_len);
        let empty: &[u8] = &[];
        let pwd_bytes = password.map(|p| p.as_bytes()).unwrap_or(empty);
        let raw = self
            .transact(&header, &[bytes, pwd_bytes], None, Self::META_TIMEOUT)
            .await?;
        let v = Self::parse_json(raw)?;
        if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
            let pages = v.get("pages").and_then(|x| x.as_u64()).unwrap_or(0) as usize;
            Ok((pages, false))
        } else if v.get("locked").and_then(|x| x.as_bool()).unwrap_or(false) {
            Ok((0, true))
        } else {
            Err(ArchiveError::Pdf(
                v.get("error")
                    .and_then(|x| x.as_str())
                    .unwrap_or("pdf failed to open")
                    .to_owned(),
            ))
        }
    }

    /// Render one page at `scale` (0.05..=8.0). Returns (width, height,
    /// RGBA bytes). Fresh shm per call: a previous page can never leak
    /// into this one, and w*h*4 must equal the byte count or the whole
    /// render is rejected, never displayed.
    pub async fn page_bitmap(
        &mut self,
        page: usize,
        scale: f32,
    ) -> ArchiveResult<(u32, u32, Vec<u8>)> {
        if page > Self::MAX_PAGE {
            return Err(ArchiveError::Pdf("page index out of range".to_string()));
        }
        if !(0.05..=8.0).contains(&scale) || !scale.is_finite() {
            return Err(ArchiveError::Pdf("render scale out of range".to_string()));
        }
        self.ensure_spawned()?;
        // Fixed 800x1100 backing: the file grows to the bitmap the child
        // writes (fs::write truncates), so scale only affects content.
        let shm = crate::shm::ShmHandle::create(800 * 1100 * 4).map_err(ArchiveError::Io)?;
        let shm_name = shm.path.to_string_lossy().to_string();
        self.shm = Some(shm);
        let path = self
            .shm
            .as_ref()
            .map(|h| h.path.clone())
            .unwrap_or_default();
        let scale_milli = (scale * 1000.0).round() as usize;
        let header = format!("PAGE {page} {scale_milli} {shm_name}\n");
        let child = match &mut self.child {
            Some(c) => c,
            None => {
                return Err(ArchiveError::Pdf(
                    "pdf renderer unavailable (spawn failed)".to_string(),
                ));
            }
        };
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        if let Some(stdin) = child.stdin.as_mut() {
            if stdin.write_all(header.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                self.shutdown();
                return Err(ArchiveError::Pdf("pdf renderer stdin failed".to_string()));
            }
        } else {
            self.shutdown();
            return Err(ArchiveError::Pdf("pdf renderer stdin failed".to_string()));
        }
        // Twelve-byte header: w, h, len. The bitmap itself travels via
        // the shm file; a short or mismatched file is a failed render,
        // never a page.
        let mut hdr = [0u8; 12];
        let stale = match &mut self.child {
            Some(child) => match child.stdout.as_mut() {
                Some(stdout) => {
                    tokio::time::timeout(Self::PAGE_TIMEOUT, stdout.read_exact(&mut hdr))
                        .await
                        .is_err()
                }
                None => true,
            },
            None => true,
        };
        if stale {
            // Wedged or dead child: kill it so the next render respawns
            // instead of queuing behind a corpse.
            self.shutdown();
            return Err(ArchiveError::Pdf("pdf renderer timed out".to_string()));
        }
        let w = u32::from_le_bytes(hdr[0..4].try_into().unwrap_or([0; 4]));
        let h = u32::from_le_bytes(hdr[4..8].try_into().unwrap_or([0; 4]));
        let len = u32::from_le_bytes(hdr[8..12].try_into().unwrap_or([0; 4])) as usize;
        if w == 0 || h == 0 || w > 8192 || h > 8192 {
            return Err(ArchiveError::Pdf(
                "pdf renderer sent bad dimensions".to_string(),
            ));
        }
        if len == 0 || len > 50 * 1024 * 1024 || len != w as usize * h as usize * 4 {
            return Err(ArchiveError::Pdf(
                "pdf renderer bitmap truncated".to_string(),
            ));
        }
        let data = std::fs::read(&path).map_err(ArchiveError::Io)?;
        if data.len() != len {
            return Err(ArchiveError::Pdf(
                "pdf renderer bitmap truncated".to_string(),
            ));
        }
        Ok((w, h, data))
    }

    async fn json_cmd(
        &mut self,
        header: &str,
        payloads: &[&[u8]],
    ) -> ArchiveResult<serde_json::Value> {
        self.ensure_spawned()?;
        let raw = self
            .transact(header, payloads, None, Self::META_TIMEOUT)
            .await?;
        Self::parse_json(raw)
    }

    /// Metadata + permissions + engine presence, one round trip.
    pub async fn doc_info(&mut self) -> ArchiveResult<serde_json::Value> {
        self.json_cmd("INFO\n", &[]).await
    }

    /// Whole-document search with per-page match rects. The child caps
    /// the page scan; `truncated` says so honestly.
    pub async fn search_all(
        &mut self,
        query: String,
        case: bool,
    ) -> ArchiveResult<serde_json::Value> {
        if query.is_empty() || query.len() > 256 {
            return Err(ArchiveError::Pdf("search query out of bounds".to_string()));
        }
        let header = format!("SEARCH {} {}\n", if case { 1 } else { 0 }, query.len());
        self.json_cmd(&header, &[query.as_bytes()]).await
    }

    /// Selectable-text lines for one page (fractions + text).
    pub async fn page_text(&mut self, page: usize) -> ArchiveResult<serde_json::Value> {
        if page > Self::MAX_PAGE {
            return Err(ArchiveError::Pdf("page index out of range".to_string()));
        }
        let header = format!("TEXT {page}\n");
        self.json_cmd(&header, &[]).await
    }

    /// Clickable regions for one page.
    pub async fn page_links(&mut self, page: usize) -> ArchiveResult<serde_json::Value> {
        if page > Self::MAX_PAGE {
            return Err(ArchiveError::Pdf("page index out of range".to_string()));
        }
        let header = format!("LINKS {page}\n");
        self.json_cmd(&header, &[]).await
    }

    pub async fn doc_outline(&mut self) -> ArchiveResult<serde_json::Value> {
        self.json_cmd("OUTLINE\n", &[]).await
    }

    pub async fn doc_forms(&mut self) -> ArchiveResult<serde_json::Value> {
        self.json_cmd("FORMS\n", &[]).await
    }

    async fn fill_cmd(&mut self, header: &str, payloads: &[&[u8]]) -> ArchiveResult<()> {
        let v = self.json_cmd(header, payloads).await?;
        if v.get("ok").and_then(|x| x.as_bool()).unwrap_or(false) {
            Ok(())
        } else {
            Err(ArchiveError::Pdf(
                v.get("error")
                    .and_then(|x| x.as_str())
                    .unwrap_or("form fill failed")
                    .to_owned(),
            ))
        }
    }

    pub async fn fill_text(&mut self, index: usize, value: String) -> ArchiveResult<()> {
        if value.len() > 16_384 {
            return Err(ArchiveError::Pdf("form value too long".to_string()));
        }
        let header = format!("FILLTEXT {index} {}\n", value.len());
        self.fill_cmd(&header, &[value.as_bytes()]).await
    }

    pub async fn fill_check(&mut self, index: usize, checked: bool) -> ArchiveResult<()> {
        let header = format!("FILLCHECK {index} {}\n", if checked { 1 } else { 0 });
        self.fill_cmd(&header, &[]).await
    }

    pub async fn fill_radio(&mut self, index: usize) -> ArchiveResult<()> {
        let header = format!("FILLRADIO {index}\n");
        self.fill_cmd(&header, &[]).await
    }

    /// Serialize the (possibly filled) document. Raw bytes, capped.
    pub async fn save_document(&mut self) -> ArchiveResult<Vec<u8>> {
        self.ensure_spawned()?;
        let raw = self
            .transact("SAVE\n", &[], None, Self::META_TIMEOUT)
            .await?;
        if raw.is_empty() || raw.len() > Self::MAX_SAVE_BYTES {
            return Err(ArchiveError::Pdf(
                "saved document out of bounds".to_string(),
            ));
        }
        Ok(raw)
    }

    pub fn is_alive(&mut self) -> bool {
        match &mut self.child {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    /// Minimal one-page PDF with a text field and rendered text. Valid
    /// enough for real pdfium (page count, search, fill, save all work)
    /// and inert enough for the fallback build (raw byte scans match).
    /// Garbage-input tests below stay fallback-only: real pdfium parses
    /// leniently (0-page docs, unenforced fake markers), so byte-soup
    /// assertions cannot hold in both builds.
    #[cfg(test)]
    fn minimal_pdf() -> &'static [u8] {
        b"%PDF-1.4\n\
            1 0 obj\n<< /Type /Catalog /Pages 2 0 R /AcroForm 4 0 R >>\nendobj\n\
            2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n\
            3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 7 0 R >> >> /Annots [5 0 R] /Contents 6 0 R >>\nendobj\n\
            4 0 obj\n<< /Fields [5 0 R] >>\nendobj\n\
            5 0 obj\n<< /Type /Annot /Subtype /Widget /Rect [100 700 300 720] /FT /Tx /T (name) /V () >>\nendobj\n\
            6 0 obj\n<< /Length 44 >>\nstream\nBT /F1 12 Tf 72 720 Td (hello world) Tj ET\nendstream\nendobj\n\
            7 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n\
            trailer\n<< /Root 1 0 R >>\n%%EOF\n"
    }

    /// Shared engine for tests: pdfium bindings initialize exactly once
    /// per process (`PdfiumLibraryBindingsAlreadyInitialized` on second
    /// bind), so every test reuses one engine. Production honors the
    /// same rule structurally: one engine per process (the renderer
    /// child owns its; the shell talks to the child, never in-process).
    #[cfg(test)]
    fn test_engine() -> &'static PdfEngine {
        static ENGINE: std::sync::OnceLock<PdfEngine> = std::sync::OnceLock::new();
        ENGINE.get_or_init(|| PdfEngine::new().expect("pdfium lib"))
    }

    // Serial: the shared engine is not safe under concurrent test use
    // (flaky native heap abort). Production never shares in-process.
    #[test]
    #[serial]
    fn load_simple_pdf() {
        let doc = test_engine().load(minimal_pdf(), None).unwrap();
        assert_eq!(doc.info().page_count, 1);
        // Rendering needs the real engine: the fallback build fails closed
        // with a named error instead of a white pixel masquerading as a page.
        #[cfg(not(feature = "pdfium-render"))]
        assert!(doc.render_page(0, 1.0).is_err());
        #[cfg(feature = "pdfium-render")]
        assert!(!doc.render_page(0, 1.0).unwrap().is_empty());
    }

    #[test]
    #[serial]
    fn password_required() {
        let engine = test_engine();
        // A literal /Encrypt marker with no real encryption: the fallback
        // fails closed on the marker; real pdfium parses leniently and
        // decides by the actual security handler (absent here).
        let pdf = b"%PDF-1.4 /Encrypt fake";
        #[cfg(not(feature = "pdfium-render"))]
        {
            assert!(engine.load(pdf, None).is_err());
            assert!(engine.load(pdf, Some("correct")).is_err());
            assert!(engine.load(pdf, Some("wrong")).is_err());
        }
        #[cfg(feature = "pdfium-render")]
        {
            // Lenient parse must never abort the process on garbage: the
            // isolated renderer child contains even a native abort, but
            // the library call itself must return (Ok or Err), not die.
            let _ = engine.load(pdf, None);
            let _ = engine.load(pdf, Some("wrong"));
        }
    }

    #[test]
    #[serial]
    fn search() {
        let doc = test_engine().load(minimal_pdf(), None).unwrap();
        assert_eq!(doc.search("hello").len(), 1);
        assert_eq!(doc.search("missing").len(), 0);
    }

    /// Fill -> save -> reload must carry the value. Runs only with the
    /// real engine (the fallback build fails fills closed by design).
    /// Needs the pdfium shared library; environments without it don't
    /// run feature tests (same as the render assertions above).
    #[cfg(feature = "pdfium-render")]
    #[test]
    #[serial]
    fn filled_text_survives_save_reload() {
        let mut doc = test_engine().load(minimal_pdf(), None).unwrap();
        assert!(!doc.is_dirty());
        doc.fill_text(0, "hello").expect("one text field");
        assert!(doc.is_dirty());
        let bytes = doc.save_bytes().expect("save filled doc");
        let doc2 = test_engine()
            .load(&bytes, None)
            .expect("reload saved bytes");
        let fields = doc2.form_fields();
        assert_eq!(fields.len(), 1, "field survives the round trip");
        assert_eq!(fields[0].value.as_deref(), Some("hello"));
    }
}

#[cfg(test)]
mod sibling_layout_tests {
    use super::*;

    /// Scratch dir under the OS temp root (std only, no new dev-deps).
    /// Unique per test via process id + name so parallel runs never share.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "ferrite-pdf-layout-{}-{}",
            std::process::id(),
            name
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    #[test]
    fn shipped_layout_reports_ready() {
        let dir = scratch("ready");
        std::fs::write(dir.join(RENDERER_EXE_NAME), b"x").expect("renderer stub");
        std::fs::write(dir.join(PDFIUM_LIB_NAME), b"x").expect("pdfium stub");
        let (renderer, pdfium) = pdf_boot_report_for_dirs(Some(dir.clone()), None);
        assert!(renderer, "renderer sibling detected");
        assert_eq!(pdfium, "sibling");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn env_override_wins_over_sibling() {
        let dir = scratch("env");
        std::fs::write(dir.join(RENDERER_EXE_NAME), b"x").expect("renderer stub");
        std::fs::write(dir.join(PDFIUM_LIB_NAME), b"x").expect("pdfium stub");
        let env = scratch("envdir");
        let (renderer, pdfium) = pdf_boot_report_for_dirs(Some(dir.clone()), Some(env.clone()));
        assert!(renderer);
        assert_eq!(pdfium, "env", "explicit operator config always wins");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&env);
    }

    #[test]
    fn bare_install_reports_degraded_honestly() {
        // v0.1.0 layout: neither file beside the exe, no env override.
        let dir = scratch("bare");
        let (renderer, pdfium) = pdf_boot_report_for_dirs(Some(dir.clone()), None);
        assert!(!renderer, "no renderer sibling");
        assert_eq!(
            pdfium, "system-or-missing",
            "never claims more than it knows"
        );
        // Undeterminable exe dir degrades the same way, never panics.
        let (r2, p2) = pdf_boot_report_for_dirs(None, None);
        assert!(!r2);
        assert_eq!(p2, "system-or-missing");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sibling_names_match_the_pinned_builds() {
        // The release workflow ships exactly these file names (bblanchon
        // 7881 layouts); a rename on either side breaks the lookup.
        assert_eq!(
            RENDERER_EXE_NAME,
            if cfg!(windows) {
                "ferrite-pdf-renderer.exe"
            } else {
                "ferrite-pdf-renderer"
            }
        );
        assert_eq!(
            PDFIUM_LIB_NAME,
            if cfg!(windows) {
                "pdfium.dll"
            } else if cfg!(target_os = "macos") {
                "libpdfium.dylib"
            } else {
                "libpdfium.so"
            }
        );
    }
}
