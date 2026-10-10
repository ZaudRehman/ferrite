//! Import and export: Netscape bookmark HTML (with real folder hierarchy),
//! Chromium `Bookmarks` JSON (folders preserved), Firefox places JSON
//! backups, and history databases with original timestamps.
//!
//! Source hygiene: SQLite files are never opened in place. Browsers hold
//! locks on them while running, so a direct open either fails or reads a
//! mid-transaction state; every reader goes through [`open_source_db`],
//! which copies to temp first. A copy taken while the browser runs can
//! still miss the last minute or two; callers say so rather than claiming
//! completeness.
//!
//! Every importer returns an [`ImportReport`] with exact counts. Repeat
//! imports are idempotent: existing bookmark URLs are skipped as
//! duplicates (folders are reused by title), history rows merge into the
//! stored visit (counts add, latest visit wins).

use rusqlite::Connection;

use crate::error::{VaultError, VaultResult};
use crate::vault::Vault;

/// Largest JSON source read whole (Chromium `Bookmarks` is text).
const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;
/// Largest SQLite source copied (live `History` files stay far below this).
const MAX_DB_BYTES: u64 = 256 * 1024 * 1024;
/// Largest HTML source read whole (Netscape exports are text).
const MAX_HTML_BYTES: u64 = 32 * 1024 * 1024;
/// Node/row ceilings so a pathological file cannot hang the loop thread.
const MAX_BOOKMARK_NODES: usize = 20_000;
const MAX_HISTORY_ROWS: usize = 100_000;
/// Title/URL length caps (URLs past this never navigate anywhere sane).
const MAX_TITLE_CHARS: usize = 500;
const MAX_URL_CHARS: usize = 2048;

/// Exact outcome of one import run. `imported` counts rows newly added;
/// `merged` counts source rows folded into an already-stored row (history
/// revisits); duplicates/invalid rows are skipped with the reason kept.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportReport {
    pub scanned: u64,
    pub imported: u64,
    pub merged: u64,
    pub skipped_duplicate: u64,
    pub skipped_invalid: u64,
    pub folders_created: u64,
}

impl ImportReport {
    /// Skipped rows regardless of reason (duplicates plus invalid).
    pub fn skipped(&self) -> u64 {
        self.skipped_duplicate.saturating_add(self.skipped_invalid)
    }

    /// One honest line for toasts and result rows, e.g.
    /// "23 bookmarks, 4 already saved, 1 skipped".
    pub fn describe(&self, singular: &str, plural: &str) -> String {
        let noun = if self.imported == 1 { singular } else { plural };
        let mut out = format!("{} {noun}", self.imported);
        if self.merged > 0 {
            out.push_str(&format!(", {} merged with saved entries", self.merged));
        }
        let skipped = self.skipped();
        if skipped > 0 {
            out.push_str(&format!(", {skipped} skipped"));
        }
        out
    }
}

/// Dispatch a bookmark file by content: Chromium JSON (`roots`), Firefox
/// places JSON (container with `children`), or Netscape HTML. Sniffs
/// content, never the extension: detected profile files arrive nameless.
pub fn import_bookmarks_file(path: &str, vault: &Vault) -> VaultResult<ImportReport> {
    let meta = std::fs::metadata(path).map_err(|e| VaultError::Import(e.to_string()))?;
    if meta.len() > MAX_JSON_BYTES.max(MAX_HTML_BYTES) {
        return Err(VaultError::Import(format!(
            "bookmarks file too large: {} bytes",
            meta.len()
        )));
    }
    let data = std::fs::read(path).map_err(|e| VaultError::Import(e.to_string()))?;
    // Firefox compressed backups are mozlz4, not JSON: say so instead of
    // misreporting "no bookmarks found".
    if data.starts_with(b"mozLz40\0") {
        return Err(VaultError::Import(
            "Firefox compressed backup (.jsonlz4): in Firefox use Bookmarks > Manage > Import and Backup > Backup… to a plain JSON file, then import that"
                .to_owned(),
        ));
    }
    let text = String::from_utf8_lossy(&data);
    let trimmed = text.trim_start();
    if trimmed.starts_with('{') {
        let v: serde_json::Value = serde_json::from_str(trimmed)
            .map_err(|e| VaultError::Import(format!("bookmarks JSON: {e}")))?;
        if v.get("roots").is_some() {
            return import_chromium_value(&v, vault);
        }
        if is_places_container(&v) {
            return import_firefox_value(&v, vault);
        }
        return Err(VaultError::Import(
            "JSON is neither Chromium Bookmarks (roots) nor a Firefox backup (places)".to_owned(),
        ));
    }
    import_netscape_text(&text, vault)
}

/// Netscape Bookmark File Format from a picked HTML export. Parses the real
/// structure: `<H3>` folders nest via the `<DL>` that follows them, links
/// carry `HREF`/`ADD_DATE`, entities decode. A file with no structure at
/// all lands in one dated folder; a file that is not bookmarks HTML errors
/// instead of reporting a misleading zero.
pub fn import_netscape_bookmarks(path: &str, vault: &Vault) -> VaultResult<ImportReport> {
    let meta = std::fs::metadata(path).map_err(|e| VaultError::Import(e.to_string()))?;
    if meta.len() > MAX_HTML_BYTES {
        return Err(VaultError::Import(format!(
            "bookmarks file too large: {} bytes",
            meta.len()
        )));
    }
    let data = std::fs::read_to_string(path).map_err(|e| VaultError::Import(e.to_string()))?;
    import_netscape_text(&data, vault)
}

fn import_netscape_text(data: &str, vault: &Vault) -> VaultResult<ImportReport> {
    let doc = parse_netscape(data);
    if doc.is_empty() && !looks_like_netscape(data) {
        return Err(VaultError::Import(
            "not a bookmarks file: no links or folders found".to_owned(),
        ));
    }
    let mut report = ImportReport::default();
    if doc.has_folders() {
        // Structured export: mirror the tree at the bar root.
        for node in &doc.roots {
            import_netscape_node(node, vault, None, &mut report);
            if report.scanned >= MAX_BOOKMARK_NODES as u64 {
                break;
            }
        }
    } else {
        // Flat list (or empty): one dated folder, reused across repeats.
        let folder = import_folder(vault)?;
        for (url, title) in doc.links() {
            report.scanned += 1;
            if report.scanned > MAX_BOOKMARK_NODES as u64 {
                break;
            }
            import_bookmark_url(url, title, Some(&folder), vault, &mut report);
        }
    }
    Ok(report)
}

/// `Imported YYYY-MM-DD` at the bar root, created on first use.
fn import_folder(vault: &Vault) -> VaultResult<String> {
    let name = {
        use std::time::{SystemTime, UNIX_EPOCH};
        let days = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() / 86_400)
            .unwrap_or(0);
        let z = days as i64 + 719_468;
        let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        format!("Imported {:04}-{:02}-{:02}", y + i64::from(m <= 2), m, d)
    };
    if let Some(id) = vault.child_folder(None, &name) {
        return Ok(id);
    }
    vault.add_folder(&name, None)
}

/// Find-or-create a child folder, counting creations. Repeat imports reuse
/// the same folders, so the tree never clones itself.
fn import_subfolder(
    vault: &Vault,
    parent: Option<&str>,
    title: &str,
    report: &mut ImportReport,
) -> VaultResult<String> {
    let title = title
        .trim()
        .chars()
        .take(MAX_TITLE_CHARS)
        .collect::<String>();
    if title.is_empty() {
        return import_folder(vault);
    }
    if let Some(id) = vault.child_folder(parent, &title) {
        return Ok(id);
    }
    let id = vault.add_folder(&title, parent)?;
    report.folders_created += 1;
    Ok(id)
}

/// Single bookmark write with validation and dedupe. Dangerous schemes
/// (`javascript:`, `data:`, …) are invalid here: they never came from a
/// navigation and must not become clickable rows.
fn import_bookmark_url(
    url: &str,
    title: &str,
    parent: Option<&str>,
    vault: &Vault,
    report: &mut ImportReport,
) {
    let url = url.trim();
    if url.is_empty() || url.chars().count() > MAX_URL_CHARS || !importable_scheme(url) {
        report.skipped_invalid += 1;
        return;
    }
    if vault.bookmark_exists(url) {
        report.skipped_duplicate += 1;
        return;
    }
    let title = title.trim();
    let title = if title.is_empty() { url } else { title };
    match vault.insert_bookmark(url, title, parent) {
        Ok(_) => report.imported += 1,
        Err(_) => report.skipped_invalid += 1,
    }
}

fn importable_scheme(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    !(lower.starts_with("javascript:")
        || lower.starts_with("data:")
        || lower.starts_with("vbscript:")
        || lower.starts_with("blob:")
        || lower.starts_with("file:")
        || lower.starts_with("place:"))
}

fn import_netscape_node(
    node: &NetscapeNode,
    vault: &Vault,
    parent: Option<&str>,
    report: &mut ImportReport,
) {
    if report.scanned >= MAX_BOOKMARK_NODES as u64 {
        return;
    }
    match node {
        NetscapeNode::Link { url, title } => {
            report.scanned += 1;
            import_bookmark_url(url, title, parent, vault, report);
        }
        NetscapeNode::Folder { title, children } => {
            let id = match import_subfolder(vault, parent, title, report) {
                Ok(id) => id,
                Err(_) => return,
            };
            for child in children {
                import_netscape_node(child, vault, Some(&id), report);
                if report.scanned >= MAX_BOOKMARK_NODES as u64 {
                    break;
                }
            }
        }
    }
}

// --- Netscape parser ------------------------------------------------------

#[derive(Debug, PartialEq)]
enum NetscapeNode {
    Link {
        url: String,
        title: String,
    },
    Folder {
        title: String,
        children: Vec<NetscapeNode>,
    },
}

struct NetscapeDoc {
    roots: Vec<NetscapeNode>,
}

impl NetscapeDoc {
    fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }

    fn has_folders(&self) -> bool {
        self.roots
            .iter()
            .any(|n| matches!(n, NetscapeNode::Folder { .. }))
    }

    fn links(&self) -> Vec<(&str, &str)> {
        let mut out = Vec::new();
        fn walk<'a>(nodes: &'a [NetscapeNode], out: &mut Vec<(&'a str, &'a str)>) {
            for n in nodes {
                match n {
                    NetscapeNode::Link { url, title } => out.push((url, title)),
                    NetscapeNode::Folder { children, .. } => walk(children, out),
                }
            }
        }
        walk(&self.roots, &mut out);
        out
    }
}

fn looks_like_netscape(data: &str) -> bool {
    let upper = data.to_ascii_uppercase();
    upper.contains("NETSCAPE") || upper.contains("<DL") || upper.contains("<DT")
}

/// Minimal tag soup parser for the Netscape format. Handles `<DT>`, `<H3
/// attrs>title</H3>` followed by its `<DL>` child list, `<A
/// HREF ADD_DATE>title</A>`, comments, and malformed nesting (stray
/// closers are ignored, unclosed lists unwind at EOF). Case-insensitive.
fn parse_netscape(data: &str) -> NetscapeDoc {
    // Each stack level: (collected nodes, folder awaiting these children).
    // A folder awaits children when its <DL> opened right after its <H3>.
    struct Level {
        nodes: Vec<NetscapeNode>,
        folder: Option<usize>,
    }
    let mut stack: Vec<Level> = vec![Level {
        nodes: Vec::new(),
        folder: None,
    }];
    let mut last_h3: Option<(usize, usize)> = None; // (level, node index)
    let bytes = data.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        // Comments and doctype: skip to the closing '>'.
        if data[i..].starts_with("<!--") {
            i = match data[i..].find("-->") {
                Some(end) => i + end + 3,
                None => break,
            };
            continue;
        }
        let tag_end = match find_tag_end(&data[i..]) {
            Some(end) => i + end,
            None => break,
        };
        let raw = data[i + 1..tag_end].trim();
        i = tag_end + 1;
        let (closing, name, attrs) = split_tag(raw);
        match name.as_str() {
            "dl" if !closing => {
                // A <DL> right after an <H3> holds that folder's children.
                let folder = last_h3.and_then(|(lvl, idx)| {
                    if lvl == stack.len() - 1 {
                        Some(idx)
                    } else {
                        None
                    }
                });
                last_h3 = None;
                stack.push(Level {
                    nodes: Vec::new(),
                    folder,
                });
            }
            "dl" => {
                if stack.len() > 1 {
                    let level = stack.pop().expect("level above root");
                    let top = stack.last_mut().expect("root level");
                    match level.folder {
                        Some(idx) => {
                            if let Some(NetscapeNode::Folder { children, .. }) =
                                top.nodes.get_mut(idx)
                            {
                                children.extend(level.nodes);
                            } else {
                                top.nodes.extend(level.nodes);
                            }
                        }
                        // Stray list with no folder: merge up, never drop.
                        None => top.nodes.extend(level.nodes),
                    }
                }
                last_h3 = None;
            }
            "h3" if !closing => {
                let title = read_text_until_close(data, &mut i, "h3");
                let title = decode_entities(&title);
                let title: String = title.trim().chars().take(MAX_TITLE_CHARS).collect();
                if !title.is_empty() {
                    let idx = {
                        let top = stack.last_mut().expect("root level");
                        top.nodes.push(NetscapeNode::Folder {
                            title,
                            children: Vec::new(),
                        });
                        top.nodes.len() - 1
                    };
                    last_h3 = Some((stack.len() - 1, idx));
                } else {
                    last_h3 = None;
                }
            }
            "a" if !closing => {
                let href = attr(attrs, "href").unwrap_or_default();
                // Attribute entities decode like element text: an
                // exported `&amp;` must come back as `&`, or every
                // export/import round-trip corrupts query strings.
                let href = decode_entities(href.as_str());
                let title = read_text_until_close(data, &mut i, "a");
                let title = decode_entities(title.as_str());
                let title: String = title.trim().chars().take(MAX_TITLE_CHARS).collect();
                let top = stack.last_mut().expect("root level");
                top.nodes.push(NetscapeNode::Link {
                    url: href.trim().to_owned(),
                    title,
                });
                last_h3 = None;
            }
            _ => {
                // DT, P, H1, closers of H3/A (consumed by the reader),
                // and anything unknown: structure-neutral.
                if name != "h3" && name != "a" {
                    // Any other opening tag breaks an H3/DL pairing.
                    if !closing {
                        last_h3 = None;
                    }
                }
            }
        }
    }
    // Unwind unclosed lists (truncated file): attach where possible.
    while stack.len() > 1 {
        let level = stack.pop().expect("level above root");
        let top = stack.last_mut().expect("root level");
        match level.folder {
            Some(idx) => {
                if let Some(NetscapeNode::Folder { children, .. }) = top.nodes.get_mut(idx) {
                    children.extend(level.nodes);
                } else {
                    top.nodes.extend(level.nodes);
                }
            }
            None => top.nodes.extend(level.nodes),
        }
    }
    NetscapeDoc {
        roots: stack.pop().expect("root level").nodes,
    }
}

/// End of a tag starting at `s[0] == '<'`, respecting quoted `>` inside
/// attribute values. Returns the index of `>`.
fn find_tag_end(s: &str) -> Option<usize> {
    let mut quote = None;
    for (idx, c) in s.char_indices() {
        if idx == 0 {
            continue;
        }
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    quote = Some(c);
                } else if c == '>' {
                    return Some(idx);
                }
            }
        }
    }
    None
}

/// Split tag innards into (closing, lowercase name, attributes). `!DOCTYPE`
/// and `?xml` shapes read as unknown names so callers ignore them.
fn split_tag(raw: &str) -> (bool, String, &str) {
    let raw = raw.trim();
    let (closing, rest) = match raw.strip_prefix('/') {
        Some(r) => (true, r.trim()),
        None => (false, raw),
    };
    let split = rest
        .find(|c: char| c.is_whitespace() || c == '/')
        .unwrap_or(rest.len());
    let (name, attrs) = rest.split_at(split);
    // `!doctype` / `?xml` never match a known name below.
    (closing, name.to_ascii_lowercase(), attrs)
}

/// Attribute value by case-insensitive name. Double-quoted, single-quoted,
/// or bare values.
fn attr(attrs: &str, want: &str) -> Option<String> {
    let bytes = attrs.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b'/') {
            i += 1;
        }
        let start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b'/'
        {
            i += 1;
        }
        if start == i {
            break;
        }
        let name = attrs[start..i].to_ascii_lowercase();
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        let mut value = String::new();
        if i < bytes.len() && bytes[i] == b'=' {
            i += 1;
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i < bytes.len() && (bytes[i] == b'"' || bytes[i] == b'\'') {
                let q = bytes[i];
                i += 1;
                let vstart = i;
                while i < bytes.len() && bytes[i] != q {
                    i += 1;
                }
                value = attrs[vstart..i].to_owned();
                i += 1;
            } else {
                let vstart = i;
                while i < bytes.len() && !bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                value = attrs[vstart..i].to_owned();
            }
        }
        if name == want {
            return Some(value);
        }
    }
    None
}

/// Text until `</name>` (case-insensitive), inner tags stripped. A new
/// structural tag (`dt`, `dl`, `h3`, `a`) ends the text too: truncated
/// files leave elements unterminated, and swallowing the rest of the file
/// into one title would lose every later bookmark. Advances `i` past the
/// real closer, or to (not past) an implicit one; unterminated with no
/// structural tag ahead reads to a length cap.
fn read_text_until_close(data: &str, i: &mut usize, name: &str) -> String {
    let lower = data.to_ascii_lowercase();
    let closer = format!("</{name}");
    let from = *i;
    let rest = &lower[from..];
    let mut end = rest.find(&closer).map(|e| from + e);
    let mut implicit = false;
    for tag in ["<dt", "<dl", "<h3", "<a"] {
        if let Some(pos) = rest.find(tag) {
            let pos = from + pos;
            // The real closer always wins ties (</a> contains "<a"... no:
            // "</a" does not contain "<a" as a substring: "</a" vs "<a":
            // "<a" needs '<' then 'a'; "</a" has '<' then '/'. No clash.
            if end.is_none_or(|e| pos < e) {
                end = Some(pos);
                implicit = true;
            }
        }
    }
    let end = end.unwrap_or_else(|| (from + 64 * 1024).min(data.len()));
    let raw = &data[from..end];
    // Consume the real closer; leave an implicit one for the main loop.
    *i = if implicit {
        end
    } else {
        data[end..]
            .find('>')
            .map(|e| end + e + 1)
            .unwrap_or(data.len())
    };
    strip_tags(raw)
}

fn strip_tags(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => {
                if depth > 0 {
                    depth -= 1;
                } else {
                    out.push(c);
                }
            }
            _ => {
                if depth == 0 {
                    out.push(c);
                }
            }
        }
    }
    out
}

/// HTML entities: named (`amp lt gt quot apos nbsp`) plus decimal and hex
/// numeric references. Unknown entities pass through untouched.
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_owned();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let tail = &rest[amp..];
        let semi = tail.find(';').filter(|&e| e <= 12);
        match semi {
            Some(e) => {
                let entity = &tail[1..e];
                if let Some(c) = decode_entity(entity) {
                    out.push(c);
                } else {
                    out.push_str(&tail[..=e]);
                }
                rest = &tail[e + 1..];
            }
            None => {
                out.push('&');
                rest = &tail[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "nbsp" => Some('\u{a0}'),
        _ => {
            if let Some(num) = entity.strip_prefix('#') {
                let (digits, radix) = match num.strip_prefix(['x', 'X']) {
                    Some(hex) => (hex, 16),
                    None => (num, 10),
                };
                u32::from_str_radix(digits, radix)
                    .ok()
                    .and_then(char::from_u32)
            } else {
                None
            }
        }
    }
}

// --- Chromium JSON ----------------------------------------------------------

/// Chromium `Bookmarks` JSON (Chrome/Edge/Brave profile file, literally
/// named `Bookmarks`: not SQLite; that is only the `History` file).
/// Folders recurse into matching Ferrite folders instead of flattening.
pub fn import_chromium_bookmarks(path: &str, vault: &Vault) -> VaultResult<ImportReport> {
    let meta = std::fs::metadata(path).map_err(|e| VaultError::Import(e.to_string()))?;
    if meta.len() > MAX_JSON_BYTES {
        return Err(VaultError::Import(format!(
            "bookmarks file too large: {} bytes",
            meta.len()
        )));
    }
    let data = std::fs::read_to_string(path).map_err(|e| VaultError::Import(e.to_string()))?;
    let v: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| VaultError::Import(format!("bookmarks JSON: {e}")))?;
    import_chromium_value(&v, vault)
}

fn import_chromium_value(v: &serde_json::Value, vault: &Vault) -> VaultResult<ImportReport> {
    let mut report = ImportReport::default();
    let Some(roots) = v.get("roots") else {
        return Err(VaultError::Import(
            "not Chromium Bookmarks: no roots object".to_owned(),
        ));
    };
    // Mirror the three roots as same-named subfolders of one dated import
    // folder, skipping empties (synced is usually vacant).
    let mut folder: Option<String> = None;
    for (key, label) in [
        ("bookmark_bar", "Bookmarks bar"),
        ("other", "Other bookmarks"),
        ("synced", "Mobile bookmarks"),
    ] {
        let Some(node) = roots.get(key) else {
            continue;
        };
        let mut entries = Vec::new();
        for child in children_of(node) {
            collect_chromium_into(child, &mut entries);
            if entries.len() >= MAX_BOOKMARK_NODES {
                break;
            }
        }
        if entries.is_empty() {
            continue;
        }
        let parent = match folder.clone() {
            Some(id) => id,
            None => {
                let id = import_folder(vault)?;
                folder = Some(id.clone());
                id
            }
        };
        let sub_id = import_subfolder(vault, Some(&parent), label, &mut report)?;
        for entry in &entries {
            import_chromium_entry(entry, vault, Some(&sub_id), &mut report);
            if report.scanned >= MAX_BOOKMARK_NODES as u64 {
                break;
            }
        }
    }
    if folder.is_none() {
        return Err(VaultError::Import(
            "Chromium Bookmarks has no bookmark_bar, other, or synced roots".to_owned(),
        ));
    }
    Ok(report)
}

enum ChromiumEntry {
    Link {
        url: String,
        title: String,
    },
    Folder {
        title: String,
        children: Vec<ChromiumEntry>,
    },
}

fn children_of(node: &serde_json::Value) -> &[serde_json::Value] {
    node.get("children")
        .and_then(|x| x.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[])
}

fn collect_chromium_into(node: &serde_json::Value, out: &mut Vec<ChromiumEntry>) {
    if out.len() >= MAX_BOOKMARK_NODES {
        return;
    }
    let kind = node.get("type").and_then(|x| x.as_str()).unwrap_or("");
    if kind == "url" {
        if let Some(url) = node.get("url").and_then(|x| x.as_str())
            && !url.is_empty()
        {
            let title = node.get("name").and_then(|x| x.as_str()).unwrap_or(url);
            out.push(ChromiumEntry::Link {
                url: url.to_owned(),
                title: title.to_owned(),
            });
        }
        return;
    }
    if node.get("children").and_then(|x| x.as_array()).is_some() {
        let title = node
            .get("name")
            .and_then(|x| x.as_str())
            .unwrap_or("Folder");
        let mut children = Vec::new();
        for c in children_of(node) {
            collect_chromium_into(c, &mut children);
            if children.len() >= MAX_BOOKMARK_NODES {
                break;
            }
        }
        out.push(ChromiumEntry::Folder {
            title: title.to_owned(),
            children,
        });
    }
}

fn import_chromium_entry(
    entry: &ChromiumEntry,
    vault: &Vault,
    parent: Option<&str>,
    report: &mut ImportReport,
) {
    if report.scanned >= MAX_BOOKMARK_NODES as u64 {
        return;
    }
    match entry {
        ChromiumEntry::Link { url, title } => {
            report.scanned += 1;
            import_bookmark_url(url, title, parent, vault, report);
        }
        ChromiumEntry::Folder { title, children } => {
            let Ok(id) = import_subfolder(vault, parent, title, report) else {
                return;
            };
            for child in children {
                import_chromium_entry(child, vault, Some(&id), report);
                if report.scanned >= MAX_BOOKMARK_NODES as u64 {
                    break;
                }
            }
        }
    }
}

// --- Firefox places JSON ----------------------------------------------------

fn is_places_container(v: &serde_json::Value) -> bool {
    v.get("type").and_then(|x| x.as_str()) == Some("text/x-moz-place-container")
        && v.get("children").and_then(|x| x.as_array()).is_some()
}

/// Firefox bookmark backup (`Bookmarks > Manage bookmarks > Import and
/// Backup > Backup…`, plain JSON). The three roots (menu, toolbar,
/// unfiled) mirror as same-named subfolders of one dated import folder.
pub fn import_firefox_bookmarks(path: &str, vault: &Vault) -> VaultResult<ImportReport> {
    let meta = std::fs::metadata(path).map_err(|e| VaultError::Import(e.to_string()))?;
    if meta.len() > MAX_JSON_BYTES {
        return Err(VaultError::Import(format!(
            "bookmarks file too large: {} bytes",
            meta.len()
        )));
    }
    let data = std::fs::read_to_string(path).map_err(|e| VaultError::Import(e.to_string()))?;
    let v: serde_json::Value = serde_json::from_str(&data)
        .map_err(|e| VaultError::Import(format!("bookmarks JSON: {e}")))?;
    import_firefox_value(&v, vault)
}

fn import_firefox_value(v: &serde_json::Value, vault: &Vault) -> VaultResult<ImportReport> {
    if !is_places_container(v) {
        return Err(VaultError::Import(
            "not a Firefox backup: no places root with children".to_owned(),
        ));
    }
    let mut report = ImportReport::default();
    let mut folder: Option<String> = None;
    let empty: Vec<serde_json::Value> = Vec::new();
    let children = v
        .get("children")
        .and_then(|x| x.as_array())
        .unwrap_or(&empty);
    for root in children {
        if !is_places_container(root) {
            continue;
        }
        let title = root
            .get("title")
            .and_then(|x| x.as_str())
            .unwrap_or("Firefox");
        let parent = match folder.clone() {
            Some(id) => id,
            None => {
                let id = import_folder(vault)?;
                folder = Some(id.clone());
                id
            }
        };
        let Ok(sub_id) = import_subfolder(vault, Some(parent.as_str()), title, &mut report) else {
            continue;
        };
        import_places_children(root, vault, &sub_id, &mut report);
        if report.scanned >= MAX_BOOKMARK_NODES as u64 {
            break;
        }
    }
    if folder.is_none() {
        return Err(VaultError::Import(
            "Firefox backup has no menu, toolbar, or unfiled roots".to_owned(),
        ));
    }
    Ok(report)
}

fn import_places_children(
    node: &serde_json::Value,
    vault: &Vault,
    parent_id: &str,
    report: &mut ImportReport,
) {
    let empty: Vec<serde_json::Value> = Vec::new();
    let children = node
        .get("children")
        .and_then(|x| x.as_array())
        .unwrap_or(&empty);
    for child in children {
        if report.scanned >= MAX_BOOKMARK_NODES as u64 {
            break;
        }
        let kind = child.get("type").and_then(|x| x.as_str()).unwrap_or("");
        match kind {
            "text/x-moz-place" => {
                let uri = child.get("uri").and_then(|x| x.as_str()).unwrap_or("");
                if uri.is_empty() {
                    report.skipped_invalid += 1;
                    continue;
                }
                report.scanned += 1;
                let title = child.get("title").and_then(|x| x.as_str()).unwrap_or(uri);
                import_bookmark_url(uri, title, Some(parent_id), vault, report);
            }
            "text/x-moz-place-container" => {
                let title = child
                    .get("title")
                    .and_then(|x| x.as_str())
                    .unwrap_or("Folder");
                let Ok(sub_id) = import_subfolder(vault, Some(parent_id), title, report) else {
                    continue;
                };
                import_places_children(child, vault, &sub_id, report);
            }
            // Separators carry no data: skip without counting invalid.
            _ => continue,
        }
    }
}

// --- History ----------------------------------------------------------------

/// Chrome/Edge `History` SQLite: `urls(url, title, visit_count,
/// last_visit_time)`. Timestamps are WebKit microseconds (since
/// 1601-01-01); original visit dates and counts survive the import.
pub fn import_chrome_history(path: &str, vault: &Vault) -> VaultResult<ImportReport> {
    let src = open_source_db(path)?;
    let mut stmt = src
        .prepare("SELECT url, title, last_visit_time, visit_count FROM urls LIMIT 100000")
        .map_err(|e| VaultError::Import(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                Some(r.get::<_, i64>(2)?),
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| VaultError::Import(e.to_string()))?;
    import_history_rows(rows, vault, webkit_to_unix_ms)
}

/// Firefox `places.sqlite`: `moz_places(url, title, lastVisitDate,
/// visit_count)`. `lastVisitDate` is PRTime (unix microseconds).
pub fn import_firefox_history(path: &str, vault: &Vault) -> VaultResult<ImportReport> {
    let src = open_source_db(path)?;
    let mut stmt = src
        .prepare("SELECT url, title, lastVisitDate, visit_count FROM moz_places WHERE url IS NOT NULL LIMIT 100000")
        .map_err(|e| VaultError::Import(e.to_string()))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| VaultError::Import(e.to_string()))?;
    import_history_rows(rows, vault, |t| t / 1000)
}

fn import_history_rows(
    rows: impl Iterator<Item = Result<(String, Option<String>, Option<i64>, i64), rusqlite::Error>>,
    vault: &Vault,
    to_ms: fn(i64) -> i64,
) -> VaultResult<ImportReport> {
    let mut report = ImportReport::default();
    for r in rows {
        if report.scanned >= MAX_HISTORY_ROWS as u64 {
            break;
        }
        let Ok((url, title, raw_ts, visits)) = r else {
            report.skipped_invalid += 1;
            continue;
        };
        if url.trim().is_empty() {
            report.skipped_invalid += 1;
            continue;
        }
        report.scanned += 1;
        let visited = raw_ts.filter(|&t| t > 0).map(to_ms);
        match vault.import_history_row(&url, title.as_deref(), "link", visited, visits) {
            Ok(true) => report.imported += 1,
            Ok(false) => report.merged += 1,
            Err(_) => report.skipped_invalid += 1,
        }
    }
    Ok(report)
}

/// WebKit microseconds (1601 epoch) to unix milliseconds.
fn webkit_to_unix_ms(t: i64) -> i64 {
    t / 1000 - 11_644_473_600_000
}

/// Safari `History.db`: `history_visits` joined to `history_items`.
/// `visit_time` is CoreData time (seconds since 2001-01-01 UTC).
pub fn import_safari_history(path: &str, vault: &Vault) -> VaultResult<ImportReport> {
    let src = open_source_db(path)?;
    let mut stmt = src
        .prepare(
            "SELECT i.url, MAX(v.title), MAX(v.visit_time), COUNT(*) \
             FROM history_visits v JOIN history_items i ON v.history_item = i.id \
             WHERE i.url IS NOT NULL GROUP BY i.url LIMIT 100000",
        )
        .map_err(|e| VaultError::Import(format!("not a Safari history database: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<f64>>(2)?
                    .map(|t| ((t + 978307200.0) * 1000.0) as i64),
                r.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| VaultError::Import(e.to_string()))?;
    import_history_rows(rows, vault, |t| t)
}

// --- Safari plist bookmarks -------------------------------------------------

/// Plist value subset for Safari `Bookmarks.plist`: dicts, arrays,
/// strings. Dates, data, numbers, and booleans parse as inert values
/// the walker ignores (they carry sync metadata, never links).
#[derive(Debug, Clone, PartialEq)]
enum Plist {
    Dict(Vec<(String, Plist)>),
    Array(Vec<Plist>),
    Str(String),
    Other,
}

struct PlistParser<'a> {
    b: &'a [u8],
    pos: usize,
    depth: usize,
    nodes: usize,
}

/// Node ceiling: the file cap bounds input, this bounds expansion.
const MAX_PLIST_NODES: usize = 500_000;
/// Nesting ceiling: real plists nest a handful deep.
const MAX_PLIST_DEPTH: usize = 64;
/// Single-string ceiling (a hostile CDATA must not balloon one title).
const MAX_PLIST_STRING: usize = 65_536;

impl<'a> PlistParser<'a> {
    fn err(&self, what: &str) -> VaultError {
        VaultError::Import(format!("Safari bookmarks plist: {what}"))
    }

    fn peek_starts(&self, s: &[u8]) -> bool {
        self.b.len() >= self.pos + s.len() && &self.b[self.pos..self.pos + s.len()] == s
    }

    fn eat(&mut self, s: &[u8]) -> bool {
        if self.peek_starts(s) {
            self.pos += s.len();
            true
        } else {
            false
        }
    }

    fn skip_ws(&mut self) {
        while self.pos < self.b.len() && self.b[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    /// Prolog, DOCTYPE, and comments: skipped, never parsed.
    fn skip_noise(&mut self) {
        loop {
            self.skip_ws();
            if self.eat(b"<!--") {
                while self.pos < self.b.len() && !self.eat(b"-->") {
                    self.pos += 1;
                }
                continue;
            }
            if self.eat(b"<?") {
                while self.pos < self.b.len() && !self.eat(b"?>") {
                    self.pos += 1;
                }
                continue;
            }
            if self.eat(b"<!DOCTYPE") {
                while self.pos < self.b.len() && !self.eat(b">") {
                    if self.b[self.pos] == b'[' {
                        // Internal subset: skip balanced brackets.
                        let mut depth = 1;
                        self.pos += 1;
                        while self.pos < self.b.len() && depth > 0 {
                            match self.b[self.pos] {
                                b'[' => depth += 1,
                                b']' => depth -= 1,
                                _ => {}
                            }
                            self.pos += 1;
                        }
                    } else {
                        self.pos += 1;
                    }
                }
                continue;
            }
            break;
        }
    }

    /// Tag name after `<` (caller consumed it): ASCII letters only,
    /// then attributes are skipped to `>` or `/>`.
    fn tag_name(&mut self) -> Result<(String, bool), VaultError> {
        let start = self.pos;
        while self.pos < self.b.len() && self.b[self.pos].is_ascii_alphabetic() {
            self.pos += 1;
        }
        let name = std::str::from_utf8(&self.b[start..self.pos])
            .map_err(|_| self.err("bad tag bytes"))?
            .to_owned();
        // Skip attributes (quoted values may contain `>`).
        let mut self_closing = false;
        loop {
            if self.pos >= self.b.len() {
                return Err(self.err("unterminated tag"));
            }
            match self.b[self.pos] {
                b'>' => {
                    self.pos += 1;
                    break;
                }
                b'/' if self.b.get(self.pos + 1) == Some(&b'>') => {
                    self.pos += 2;
                    self_closing = true;
                    break;
                }
                b'"' | b'\'' => {
                    let q = self.b[self.pos];
                    self.pos += 1;
                    while self.pos < self.b.len() && self.b[self.pos] != q {
                        self.pos += 1;
                    }
                    if self.pos < self.b.len() {
                        self.pos += 1;
                    }
                }
                _ => self.pos += 1,
            }
        }
        Ok((name, self_closing))
    }

    fn expect_close(&mut self, name: &str) -> Result<(), VaultError> {
        self.skip_ws();
        if !self.eat(b"</") {
            return Err(self.err(&format!("expected </{name}>")));
        }
        let start = self.pos;
        while self.pos < self.b.len() && self.b[self.pos].is_ascii_alphabetic() {
            self.pos += 1;
        }
        let got =
            std::str::from_utf8(&self.b[start..self.pos]).map_err(|_| self.err("bad close tag"))?;
        if got != name {
            return Err(self.err(&format!("mismatched close </{got}>, want </{name}>")));
        }
        self.skip_ws();
        if !self.eat(b">") {
            return Err(self.err("unterminated close tag"));
        }
        Ok(())
    }

    /// Text until `<`, entity-decoded, capped.
    fn text(&mut self) -> Result<String, VaultError> {
        let start = self.pos;
        while self.pos < self.b.len() && self.b[self.pos] != b'<' {
            self.pos += 1;
        }
        let raw = std::str::from_utf8(&self.b[start..self.pos])
            .map_err(|_| self.err("bad text bytes"))?;
        decode_plist_entities(raw, MAX_PLIST_STRING).ok_or_else(|| self.err("bad entity"))
    }

    fn parse_value(&mut self) -> Result<Plist, VaultError> {
        self.skip_noise();
        if self.pos >= self.b.len() || self.b[self.pos] != b'<' {
            return Err(self.err("expected element"));
        }
        // Closing tags never start a value.
        if self.b.get(self.pos + 1) == Some(&b'/') {
            return Err(self.err("stray close tag"));
        }
        self.pos += 1;
        let (name, self_closing) = self.tag_name()?;
        self.nodes += 1;
        if self.nodes > MAX_PLIST_NODES {
            return Err(self.err("too many nodes"));
        }
        if self_closing {
            return Ok(match name.as_str() {
                "dict" => Plist::Dict(Vec::new()),
                "array" => Plist::Array(Vec::new()),
                "string" => Plist::Str(String::new()),
                _ => Plist::Other,
            });
        }
        match name.as_str() {
            "dict" => {
                if self.depth >= MAX_PLIST_DEPTH {
                    return Err(self.err("too deep"));
                }
                self.depth += 1;
                let mut pairs = Vec::new();
                loop {
                    self.skip_noise();
                    if self.peek_starts(b"</") {
                        break;
                    }
                    if self.pos >= self.b.len() || self.b[self.pos] != b'<' {
                        return Err(self.err("expected key"));
                    }
                    self.pos += 1;
                    let (kname, kself) = self.tag_name()?;
                    if kname != "key" || kself {
                        return Err(self.err("expected <key>"));
                    }
                    let key = self.text()?;
                    self.expect_close("key")?;
                    let value = self.parse_value()?;
                    pairs.push((key, value));
                }
                self.depth -= 1;
                self.expect_close("dict")?;
                Ok(Plist::Dict(pairs))
            }
            "array" => {
                if self.depth >= MAX_PLIST_DEPTH {
                    return Err(self.err("too deep"));
                }
                self.depth += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_noise();
                    if self.peek_starts(b"</") {
                        break;
                    }
                    items.push(self.parse_value()?);
                }
                self.depth -= 1;
                self.expect_close("array")?;
                Ok(Plist::Array(items))
            }
            "string" => {
                let s = self.text()?;
                self.expect_close("string")?;
                Ok(Plist::Str(s))
            }
            // The document wrapper: exactly one value child, then close.
            // Attributes (version) were already skipped by tag_name.
            "plist" => {
                if self.depth > 0 {
                    return Err(self.err("nested plist"));
                }
                self.depth += 1;
                let v = self.parse_value()?;
                self.depth -= 1;
                self.skip_noise();
                self.expect_close("plist")?;
                Ok(v)
            }
            _ => {
                // Leaf scalars only (integer, date, data, real): text to
                // the close tag, then match it. Unknown containers are
                // not a Safari shape; error rather than mis-skip.
                while self.pos < self.b.len() && self.b[self.pos] != b'<' {
                    self.pos += 1;
                }
                self.expect_close(&name)?;
                Ok(Plist::Other)
            }
        }
    }
}

/// XML entities for plist text: the five predefined plus decimal/hex
/// character references. None when malformed or over the cap.
fn decode_plist_entities(s: &str, cap: usize) -> Option<String> {
    let mut out = String::with_capacity(s.len().min(cap));
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if out.len() >= cap {
            return None;
        }
        if c != '&' {
            out.push(c);
            continue;
        }
        let mut ent = String::new();
        while let Some(&d) = chars.peek() {
            chars.next();
            if d == ';' {
                break;
            }
            ent.push(d);
            if ent.len() > 12 {
                return None;
            }
        }
        match ent.as_str() {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            _ if ent.starts_with("#x") || ent.starts_with("#X") => {
                let n = u32::from_str_radix(ent.trim_start_matches(['#', 'x', 'X']), 16).ok()?;
                out.push(char::from_u32(n)?);
            }
            _ if ent.starts_with('#') => {
                let n: u32 = ent[1..].parse().ok()?;
                out.push(char::from_u32(n)?);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn plist_dict_title(pairs: &[(String, Plist)]) -> Option<String> {
    let pick = |key: &str| {
        pairs
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| match v {
                Plist::Str(t) if !t.trim().is_empty() => Some(t.clone()),
                _ => None,
            })
    };
    if let Some(t) = pick("Title") {
        return Some(t);
    }
    // Some entries carry the display title under URIDictionary.
    if let Some(Plist::Dict(inner)) = pairs
        .iter()
        .find(|(k, _)| k == "URIDictionary")
        .map(|(_, v)| v)
        && let Some(t) = inner
            .iter()
            .find(|(k, _)| k == "title")
            .and_then(|(_, v)| match v {
                Plist::Str(t) if !t.trim().is_empty() => Some(t.clone()),
                _ => None,
            })
    {
        return Some(t);
    }
    None
}

/// (title, url, folder path) leaves under Children trees. Folders nest
/// by Title; leaves need URLString. Reading-list and other special
/// trees import as ordinary bookmarks (they are user-saved URLs; the
/// folder path says where they lived).
fn safari_leaves(
    node: &Plist,
    path: &mut Vec<String>,
    out: &mut Vec<(String, String, Vec<String>)>,
) {
    if out.len() >= MAX_BOOKMARK_NODES {
        return;
    }
    match node {
        Plist::Dict(pairs) => {
            let get = |k: &str| pairs.iter().find(|(kk, _)| kk == k).map(|(_, v)| v);
            if let Some(Plist::Str(url)) = get("URLString") {
                if !url.trim().is_empty() {
                    let title = plist_dict_title(pairs).unwrap_or_else(|| url.clone());
                    out.push((title, url.clone(), path.clone()));
                }
                return;
            }
            if let Some(Plist::Array(kids)) = get("Children") {
                let mut sub = path.clone();
                if let Some(t) = plist_dict_title(pairs) {
                    // Top-level container titles (BookmarksBar etc.) are
                    // Safari chrome, not user folders: skip the first level.
                    if !path.is_empty() || (t != "BookmarksBar" && t != "BookmarksMenu") {
                        sub.push(t);
                    }
                }
                for k in kids {
                    safari_leaves(k, &mut sub, out);
                }
            }
        }
        Plist::Array(items) => {
            for it in items {
                safari_leaves(it, path, out);
            }
        }
        _ => {}
    }
}

/// Safari `Bookmarks.plist`: folder tree mirrored, entities decoded,
/// duplicates skipped like every other importer. Repeat imports reuse
/// the same folders.
pub fn import_safari_plist(path: &str, vault: &Vault) -> VaultResult<ImportReport> {
    let meta = std::fs::metadata(path).map_err(|e| VaultError::Import(e.to_string()))?;
    if meta.len() > MAX_JSON_BYTES {
        return Err(VaultError::Import(format!(
            "bookmarks file too large: {} bytes",
            meta.len()
        )));
    }
    let data = std::fs::read(path).map_err(|e| VaultError::Import(e.to_string()))?;
    // Binary plists (`bplist00`) are out of scope: Safari writes XML for
    // Bookmarks.plist; say so instead of misreporting emptiness.
    if data.starts_with(b"bplist00") {
        return Err(VaultError::Import(
            "binary plist: this Safari version stores bookmarks in a binary plist Ferrite cannot read. Export bookmarks as HTML from Safari (File > Export Bookmarks) and import that".to_owned(),
        ));
    }
    let mut parser = PlistParser {
        b: &data,
        pos: 0,
        depth: 0,
        nodes: 0,
    };
    let root = parser.parse_value()?;
    let Plist::Dict(pairs) = root else {
        return Err(VaultError::Import(
            "not a Safari bookmarks plist (root is not a dict)".to_owned(),
        ));
    };
    let children = pairs
        .iter()
        .find(|(k, _)| k == "Children")
        .and_then(|(_, v)| match v {
            Plist::Array(items) => Some(items.clone()),
            _ => None,
        })
        .ok_or_else(|| {
            VaultError::Import("not a Safari bookmarks plist (no Children)".to_owned())
        })?;
    let mut leaves = Vec::new();
    let mut path = Vec::new();
    for c in &children {
        safari_leaves(c, &mut path, &mut leaves);
    }
    if leaves.is_empty() {
        return Err(VaultError::Import("no bookmarks found in plist".to_owned()));
    }
    let mut report = ImportReport::default();
    for (title, url, folders) in &leaves {
        if report.scanned >= MAX_BOOKMARK_NODES as u64 {
            break;
        }
        report.scanned += 1;
        let mut parent: Option<String> = None;
        for f in folders {
            match import_subfolder(vault, parent.as_deref(), f, &mut report) {
                Ok(id) => parent = Some(id),
                Err(_) => {
                    report.skipped_invalid += 1;
                    parent = None;
                    break;
                }
            }
        }
        import_bookmark_url(url, title, parent.as_deref(), vault, &mut report);
    }
    Ok(report)
}

// --- Chromium site searches + addresses -------------------------------------

/// One site search from a Chromium `Web Data` `keywords` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportedEngine {
    pub name: String,
    pub template: String,
}

/// Read site searches from a Chromium `Web Data` file (Chrome, Edge:
/// same schema). Templates must carry `%s` and start with http(s);
/// anything else is skipped, never stored.
pub fn read_chrome_engines(path: &str) -> VaultResult<Vec<ImportedEngine>> {
    let src = open_source_db(path)?;
    let mut stmt = src
        .prepare("SELECT short_name, keyword, url FROM keywords LIMIT 500")
        .map_err(|e| VaultError::Import(format!("not a Chromium Web Data file: {e}")))?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|e| VaultError::Import(e.to_string()))?;
    let mut out = Vec::new();
    for r in rows {
        let Ok((short, keyword, url)) = r else {
            continue;
        };
        let name = short
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| keyword.as_deref().filter(|s| !s.trim().is_empty()))
            .unwrap_or("")
            .trim()
            .to_owned();
        let template = url.unwrap_or_default().trim().to_owned();
        if name.is_empty()
            || !template.contains("%s")
            || !(template.starts_with("https://") || template.starts_with("http://"))
            || template.len() > MAX_URL_CHARS
        {
            continue;
        }
        if out.len() >= 500 {
            break;
        }
        out.push(ImportedEngine { name, template });
    }
    Ok(out)
}

/// One address profile from a Chromium `Web Data` `autofill_profiles`
/// row. Cards and CVCs are never read (different table, encrypted at
/// rest by the OS keychain: out of scope by decision).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChromeAddress {
    pub name: String,
    pub street: String,
    pub city: String,
    pub region: String,
    pub postal: String,
    pub country: String,
    pub phone: String,
    pub email: String,
}

/// Read address profiles from a Chromium `Web Data` file. Schema drift
/// across Chromium versions fails closed with guidance instead of a
/// half-mapped row: the explicit column list must prepare cleanly.
pub fn read_chrome_addresses(path: &str) -> VaultResult<Vec<ChromeAddress>> {
    const COLS: &str = "full_name, first_name, last_name, street_address, city, state, zipcode, country_code, phone_home_whole_number";
    let src = open_source_db(path)?;
    // Email lives in a sibling table; the join is optional (older
    // schemas lack it): try with, fall back without.
    let with_email = format!(
        "SELECT {COLS}, (SELECT email FROM autofill_profile_emails e WHERE e.profile_guid = p.guid LIMIT 1) \
         FROM autofill_profiles p LIMIT 500"
    );
    let bare = format!("SELECT {COLS} FROM autofill_profiles LIMIT 500");
    let mut stmt = src
        .prepare(&with_email)
        .or_else(|_| src.prepare(&bare))
        .map_err(|_| {
            VaultError::Import(
                "not a Chromium Web Data file (autofill_profiles unreadable)".to_owned(),
            )
        })?;
    let rows = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, Option<String>>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<String>>(7)?,
                r.get::<_, Option<String>>(8)?,
                r.get::<_, Option<String>>(9).unwrap_or(None),
            ))
        })
        .map_err(|e| VaultError::Import(e.to_string()))?;
    let mut out = Vec::new();
    for r in rows {
        let Ok((full, first, last, street, city, state, zip, country, phone, email)) = r else {
            continue;
        };
        let clean = |o: Option<String>| o.unwrap_or_default().trim().to_owned();
        let (full, first, last) = (clean(full), clean(first), clean(last));
        let name = if !full.is_empty() {
            full
        } else {
            format!("{} {}", first, last).trim().to_owned()
        };
        let addr = ChromeAddress {
            name,
            street: clean(street),
            city: clean(city),
            region: clean(state),
            postal: clean(zip),
            country: clean(country),
            phone: clean(phone),
            email: clean(email),
        };
        if addr.name.is_empty()
            && addr.street.is_empty()
            && addr.city.is_empty()
            && addr.postal.is_empty()
            && addr.phone.is_empty()
            && addr.email.is_empty()
        {
            continue;
        }
        if out.len() >= 500 {
            break;
        }
        out.push(addr);
    }
    Ok(out)
}

/// Copy a source SQLite file to temp and open the copy. Never opens the
/// live file: the owning browser may hold it locked (open fails) or be
/// mid-transaction (torn read). The temp file is removed when the
/// returned handle drops.
pub fn open_source_db(path: &str) -> VaultResult<TempDb> {
    let meta = std::fs::metadata(path).map_err(|e| VaultError::Import(e.to_string()))?;
    if meta.len() > MAX_DB_BYTES {
        return Err(VaultError::Import(format!(
            "database too large: {} bytes",
            meta.len()
        )));
    }
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dest = std::env::temp_dir().join(format!("ferrite-import-{}-{}.db", std::process::id(), n));
    std::fs::copy(path, &dest).map_err(|e| {
        VaultError::Import(format!("copy source database (close the browser?): {e}"))
    })?;
    let conn = Connection::open(&dest).map_err(|e| {
        let _ = std::fs::remove_file(&dest);
        VaultError::Import(e.to_string())
    })?;
    Ok(TempDb {
        conn: Some(conn),
        path: dest,
    })
}

/// Owned handle on a temp-copied source database. Dropping closes the
/// connection first (releasing the file), then removes the temp file.
pub struct TempDb {
    conn: Option<Connection>,
    path: std::path::PathBuf,
}

impl TempDb {
    fn prepare(&self, sql: &str) -> VaultResult<rusqlite::Statement<'_>> {
        let Some(conn) = self.conn.as_ref() else {
            return Err(VaultError::Import("temp database closed".to_owned()));
        };
        conn.prepare(sql)
            .map_err(|e| VaultError::Import(e.to_string()))
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        self.conn.take();
        let _ = std::fs::remove_file(&self.path);
    }
}

// --- Export -----------------------------------------------------------------

fn esc_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Netscape export with the real folder tree: folders nest as `<H3>` plus
/// child `<DL>`, links carry escaped `HREF`s. Round-trips through
/// [`import_netscape_bookmarks`] with structure intact, so sealed backups
/// and portable tarballs preserve folders too.
pub fn export_bookmarks_netscape(vault: &Vault) -> String {
    let mut out = String::from(
        "<!DOCTYPE NETSCAPE-Bookmark-file-1>\n<!-- Exported by Ferrite -->\n<TITLE>Bookmarks</TITLE>\n<H1>Bookmarks</H1>\n<DL><p>\n",
    );
    export_bookmark_level(vault, None, &mut out);
    out.push_str("</DL><p>\n");
    out
}

fn export_bookmark_level(vault: &Vault, parent: Option<&str>, out: &mut String) {
    let now_s = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    for node in vault.list_bookmark_children(parent) {
        if node.is_folder {
            out.push_str(&format!(
                "    <DT><H3 ADD_DATE=\"{now_s}\">{}</H3>\n    <DL><p>\n",
                esc_html(&node.title)
            ));
            export_bookmark_level(vault, Some(&node.id), out);
            out.push_str("    </DL><p>\n");
        } else {
            out.push_str(&format!(
                "    <DT><A HREF=\"{}\">{}</A>\n",
                esc_html(&node.url),
                esc_html(&node.title)
            ));
        }
    }
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_owned()
    }
}

/// Portable history export: `url,title,last_visit_ms,visit_count` rows,
/// newest first. Unix-millisecond timestamps re-import losslessly through
/// any tool that keeps the columns.
pub fn export_history_csv(vault: &Vault, limit: usize) -> String {
    let mut out = String::from("url,title,last_visit_ms,visit_count\n");
    for e in vault.export_history_rows(limit) {
        out.push_str(&format!(
            "{},{},{},{}\n",
            csv_field(&e.url),
            csv_field(&e.title.unwrap_or_default()),
            e.last_visit,
            e.visit_count,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_temp(name: &str, data: &[u8]) -> String {
        let path = std::env::temp_dir().join(name);
        std::fs::write(&path, data).unwrap();
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn netscape_hierarchy_entities_and_safety() {
        let html = r#"<!DOCTYPE NETSCAPE-Bookmark-file-1>
<TITLE>Bookmarks</TITLE><H1>Bookmarks</H1>
<DL><p>
<DT><H3 ADD_DATE="1704067200">Work &amp; Play</H3>
<DL><p>
<DT><A HREF="https://a.test/x?a=1&amp;b=2">A &lt;tag&gt;</A>
<DT><H3>Empty</H3>
<DL><p>
</DL><p>
<DT><A HREF="javascript:alert(1)">evil</A>
<DT><A HREF="place:query">smart</A>
</DL><p>
<DT><A HREF="https://root.test/">Root</A>
</DL><p>"#;
        let path = write_temp("ferrite_test_netscape_hier.html", html.as_bytes());
        let v = Vault::open_in_memory().unwrap();
        let r = import_netscape_bookmarks(&path, &v).unwrap();
        // 4 links scanned: 2 good, javascript: and place: rejected.
        assert_eq!(r.scanned, 4);
        assert_eq!(r.imported, 2);
        assert_eq!(r.skipped_invalid, 2);
        assert_eq!(r.folders_created, 2);
        // Entities decoded, structure mirrored.
        let work = v.child_folder(None, "Work & Play").expect("decoded folder");
        let kids = v.list_bookmark_children(Some(&work));
        assert!(
            kids.iter()
                .any(|n| n.url.contains("&b=2") && n.title == "A <tag>")
        );
        assert!(v.child_folder(Some(&work), "Empty").is_some());
        assert!(v.bookmark_cache.contains_key("https://root.test/"));
        // Repeat: same folders reused, links skipped as duplicates.
        let again = import_netscape_bookmarks(&path, &v).unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.skipped_duplicate, 2);
        assert_eq!(v.bookmark_folders().len(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn netscape_garbage_errors_truncated_survives() {
        let v = Vault::open_in_memory().unwrap();
        let bad = write_temp("ferrite_test_netscape_bad.txt", b"just some text\n");
        assert!(import_netscape_bookmarks(&bad, &v).is_err());
        let _ = std::fs::remove_file(&bad);
        // Truncated mid-list still keeps what parsed.
        let cut = "<DL><p><DT><H3>Half<DL><p><DT><A HREF=\"https://half.test/\">Half";
        let path = write_temp("ferrite_test_netscape_cut.html", cut.as_bytes());
        let r = import_netscape_bookmarks(&path, &v).unwrap();
        assert_eq!(r.imported, 1);
        assert!(v.bookmark_cache.contains_key("https://half.test/"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn firefox_places_json_imports() {
        let json = r#"{"title":"","id":1,"type":"text/x-moz-place-container","root":"placesRoot","children":[
            {"title":"Bookmarks Menu","id":2,"type":"text/x-moz-place-container","root":"placesMenu","children":[
                {"title":"Docs","id":3,"type":"text/x-moz-place","uri":"https://docs.test/"},
                {"title":"Sub","id":4,"type":"text/x-moz-place-container","children":[
                    {"title":"Deep","id":5,"type":"text/x-moz-place","uri":"https://deep.test/"}
                ]},
                {"title":"","id":6,"type":"text/x-moz-place-separator"}
            ]},
            {"title":"Bookmarks Toolbar","id":7,"type":"text/x-moz-place-container","root":"placesToolbar","children":[]}
        ]}"#;
        let path = write_temp("ferrite_test_places.json", json.as_bytes());
        let v = Vault::open_in_memory().unwrap();
        let r = import_firefox_bookmarks(&path, &v).unwrap();
        assert_eq!(r.imported, 2);
        assert!(v.bookmark_cache.contains_key("https://deep.test/"));
        let menu = v
            .bookmark_folders()
            .into_iter()
            .find(|f| f.title == "Bookmarks Menu")
            .expect("menu mirror");
        assert!(v.child_folder(Some(&menu.id), "Sub").is_some());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn dispatcher_routes_by_content() {
        let v = Vault::open_in_memory().unwrap();
        let chrome = write_temp(
            "ferrite_test_disp_chrome.json",
            br#"{"roots":{"bookmark_bar":{"children":[{"type":"url","name":"C","url":"https://c.test/"}]}}}"#,
        );
        assert_eq!(import_bookmarks_file(&chrome, &v).unwrap().imported, 1);
        let places = write_temp(
            "ferrite_test_disp_places.json",
            br#"{"title":"","type":"text/x-moz-place-container","children":[{"title":"M","type":"text/x-moz-place-container","children":[{"title":"P","type":"text/x-moz-place","uri":"https://p.test/"}]}]}"#,
        );
        assert_eq!(import_bookmarks_file(&places, &v).unwrap().imported, 1);
        let html = write_temp(
            "ferrite_test_disp.html",
            b"<DL><p><DT><A HREF=\"https://h.test/\">H</A></DL><p>",
        );
        assert_eq!(import_bookmarks_file(&html, &v).unwrap().imported, 1);
        // Unknown JSON shape and compressed backups error honestly.
        let weird = write_temp("ferrite_test_disp_weird.json", br#"{"a":1}"#);
        assert!(import_bookmarks_file(&weird, &v).is_err());
        let lz4 = write_temp("ferrite_test_disp.jsonlz4", b"mozLz40\0rest");
        assert!(
            import_bookmarks_file(&lz4, &v)
                .unwrap_err()
                .to_string()
                .contains("jsonlz4")
        );
        for p in [chrome, places, html, weird, lz4] {
            let _ = std::fs::remove_file(p);
        }
    }

    #[test]
    fn export_round_trips_hierarchy() {
        let v = Vault::open_in_memory().unwrap();
        let f = v.add_folder("Odd <Fol> & Co", None).unwrap();
        v.insert_bookmark("https://a.test/?x=1&y=2", "A \"quoted\"", Some(&f))
            .unwrap();
        v.insert_bookmark("https://plain.test/", "Plain", None)
            .unwrap();
        let html = export_bookmarks_netscape(&v);
        assert!(html.contains("<H3"), "folders nest");
        assert!(html.contains("&amp;") && html.contains("&lt;"), "escaped");
        // Re-import into a fresh vault: same tree, decoded text.
        let path = write_temp("ferrite_test_roundtrip.html", html.as_bytes());
        let v2 = Vault::open_in_memory().unwrap();
        let r = import_netscape_bookmarks(&path, &v2).unwrap();
        assert_eq!(r.imported, 2);
        let f2 = v2
            .child_folder(None, "Odd <Fol> & Co")
            .expect("folder back");
        let kids = v2.list_bookmark_children(Some(&f2));
        assert_eq!(kids.len(), 1);
        assert_eq!(kids[0].url, "https://a.test/?x=1&y=2");
        assert_eq!(kids[0].title, "A \"quoted\"");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn history_csv_exports_newest_first() {
        let v = Vault::open_in_memory().unwrap();
        // Fixed past timestamps (2023-11 / 2025-06): stored verbatim.
        v.import_history_row(
            "https://old.test/",
            Some("Old"),
            "link",
            Some(1_700_000_000_000),
            2,
        )
        .unwrap();
        v.import_history_row(
            "https://new.test/",
            Some("New, Inc"),
            "link",
            Some(1_750_000_000_000),
            5,
        )
        .unwrap();
        let csv = export_history_csv(&v, 100_000);
        let mut lines = csv.lines();
        assert_eq!(lines.next().unwrap(), "url,title,last_visit_ms,visit_count");
        let first = lines.next().unwrap();
        assert!(first.starts_with("https://new.test/,\"New, Inc\",1750000000000,5"));
        assert!(csv.contains("https://old.test/,Old,1700000000000,2"));
    }

    #[test]
    fn report_describes_honestly() {
        let r = ImportReport {
            scanned: 10,
            imported: 1,
            merged: 2,
            skipped_duplicate: 3,
            skipped_invalid: 4,
            folders_created: 1,
        };
        assert_eq!(
            r.describe("bookmark", "bookmarks"),
            "1 bookmark, 2 merged with saved entries, 7 skipped"
        );
        assert_eq!(ImportReport::default().describe("row", "rows"), "0 rows");
    }

    #[test]
    fn webkit_epoch_converts() {
        // 2024-01-01T00:00Z.
        assert_eq!(webkit_to_unix_ms(13_348_540_800_000_000), 1_704_067_200_000);
    }

    #[test]
    fn import_clamps_wild_timestamps() {
        let v = Vault::open_in_memory().unwrap();
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap();
        // Far future lands on tomorrow, not 5138.
        v.import_history_row(
            "https://future.test/",
            None,
            "link",
            Some(9_999_999_999_999),
            1,
        )
        .unwrap();
        let row = v.history_row_by_url("https://future.test/").unwrap();
        assert!(row.last_visit >= now - 60_000 && row.last_visit <= now + 86_400_000);
        // Pre-2000 sorts as ancient, never faked as recent.
        v.import_history_row("https://ancient.test/", None, "link", Some(123), 1)
            .unwrap();
        let row = v.history_row_by_url("https://ancient.test/").unwrap();
        assert_eq!(row.last_visit, 946_684_800_000);
    }

    const SAFARI_PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
<key>Children</key>
<array>
<dict>
<key>Title</key><string>BookmarksBar</string>
<key>Children</key>
<array>
<dict><key>Title</key><string>Work &amp; Play</string><key>Children</key><array>
<dict><key>Title</key><string>A &lt;tag&gt;</string><key>URLString</key><string>https://a.test/x?a=1&amp;b=2</string></dict>
<dict><key>URIDictionary</key><dict><key>title</key><string>Dict Title</string></dict><key>URLString</key><string>https://d.test/</string></dict>
<dict><key>Title</key><string>evil</string><key>URLString</key><string>javascript:alert(1)</string></dict>
</array></dict>
<dict><key>Title</key><string>Root</string><key>URLString</key><string>https://root.test/</string></dict>
</array>
</dict>
<dict>
<key>Title</key><string>BookmarksMenu</string>
<key>Children</key><array>
<dict><key>Title</key><string>Menu Link</string><key>URLString</key><string>https://m.test/</string></dict>
</array></dict>
</array>
</dict>
</plist>"#;

    #[test]
    fn safari_plist_mirrors_folders_and_entities() {
        let path = write_temp("ferrite_test_safari.plist", SAFARI_PLIST.as_bytes());
        let v = Vault::open_in_memory().unwrap();
        let r = import_safari_plist(&path, &v).unwrap();
        // 5 leaves scanned: 4 good, javascript: rejected.
        assert_eq!(r.scanned, 5);
        assert_eq!(r.imported, 4);
        assert_eq!(r.skipped_invalid, 1);
        // Chrome bar/menu containers skipped; user folder mirrored.
        let work = v.child_folder(None, "Work & Play").expect("decoded folder");
        let kids = v.list_bookmark_children(Some(&work));
        assert_eq!(kids.len(), 2);
        assert!(
            kids.iter()
                .any(|n| n.url.contains("&b=2") && n.title == "A <tag>")
        );
        assert!(kids.iter().any(|n| n.title == "Dict Title"));
        assert!(v.bookmark_cache.contains_key("https://root.test/"));
        assert!(v.bookmark_cache.contains_key("https://m.test/"));
        // Repeat: duplicates skipped, folders reused.
        let again = import_safari_plist(&path, &v).unwrap();
        assert_eq!(again.imported, 0);
        assert_eq!(again.skipped_duplicate, 4);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn safari_plist_rejects_garbage_and_binary() {
        let v = Vault::open_in_memory().unwrap();
        let bad = write_temp(
            "ferrite_test_safari_bad.plist",
            b"<dict><key>Children</key></dict>",
        );
        assert!(import_safari_plist(&bad, &v).is_err());
        let trunc = write_temp(
            "ferrite_test_safari_trunc.plist",
            b"<?xml version=\"1.0\"?><plist><dict>",
        );
        assert!(import_safari_plist(&trunc, &v).is_err());
        let mut bin = b"bplist00".to_vec();
        bin.extend_from_slice(&[0u8; 64]);
        let binpath = write_temp("ferrite_test_safari_bin.plist", &bin);
        let err = import_safari_plist(&binpath, &v).unwrap_err().to_string();
        assert!(
            err.contains("binary plist"),
            "guides to HTML export, got: {err}"
        );
        for p in [bad, trunc, binpath] {
            let _ = std::fs::remove_file(p);
        }
    }

    fn make_sqlite(name: &str, sql: &[&str]) -> String {
        let path = std::env::temp_dir().join(name);
        let _ = std::fs::remove_file(&path);
        let conn = rusqlite::Connection::open(&path).unwrap();
        for s in sql {
            conn.execute_batch(s).unwrap();
        }
        drop(conn);
        path.to_str().unwrap().to_owned()
    }

    #[test]
    fn safari_history_converts_coredata_time() {
        // 2024-01-01T00:00Z unix 1704067200 = CoreData 725760000.
        let path = make_sqlite(
            "ferrite_test_safari_hist.db",
            &[
                "CREATE TABLE history_items(id INTEGER PRIMARY KEY, url TEXT);
                 CREATE TABLE history_visits(id INTEGER PRIMARY KEY, history_item INTEGER, title TEXT, visit_time REAL);",
                "INSERT INTO history_items(id, url) VALUES(1, 'https://s.test/a');
                 INSERT INTO history_visits(history_item, title, visit_time) VALUES(1, 'A', 725760000.0);
                 INSERT INTO history_visits(history_item, title, visit_time) VALUES(1, 'A', 725760060.0);",
            ],
        );
        let v = Vault::open_in_memory().unwrap();
        let r = import_safari_history(&path, &v).unwrap();
        assert_eq!(r.imported, 1);
        let row = v.history_row_by_url("https://s.test/a").unwrap();
        assert_eq!(row.last_visit, 1_704_067_260_000);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn safari_history_rejects_wrong_schema() {
        let path = make_sqlite(
            "ferrite_test_safari_wrong.db",
            &["CREATE TABLE urls(url TEXT);"],
        );
        let v = Vault::open_in_memory().unwrap();
        let err = import_safari_history(&path, &v).unwrap_err().to_string();
        assert!(err.contains("Safari history"), "got: {err}");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn chrome_engines_and_addresses_read() {
        let path = make_sqlite(
            "ferrite_test_webdata.db",
            &[
                "CREATE TABLE keywords(id INTEGER PRIMARY KEY, short_name TEXT, keyword TEXT, url TEXT);
                 INSERT INTO keywords(short_name, keyword, url) VALUES('GitHub', 'gh', 'https://github.com/search?q=%s');
                 INSERT INTO keywords(short_name, keyword, url) VALUES('', 'bad', 'https://x.test/noquery');
                 INSERT INTO keywords(short_name, keyword, url) VALUES('Evil', 'ev', 'javascript:do(%s)');
                 CREATE TABLE autofill_profiles(guid TEXT, full_name TEXT, first_name TEXT, last_name TEXT, street_address TEXT, city TEXT, state TEXT, zipcode TEXT, country_code TEXT, phone_home_whole_number TEXT);
                 CREATE TABLE autofill_profile_emails(profile_guid TEXT, email TEXT);
                 INSERT INTO autofill_profiles(guid, full_name, street_address, city, state, zipcode, country_code, phone_home_whole_number) VALUES('g1', 'Ada L', '1 Main St', 'Dessau', 'ST', '06844', 'DE', '+49123');
                 INSERT INTO autofill_profile_emails(profile_guid, email) VALUES('g1', 'ada@test.example');",
            ],
        );
        let engines = read_chrome_engines(&path).unwrap();
        assert_eq!(engines.len(), 1);
        assert_eq!(engines[0].name, "GitHub");
        assert!(engines[0].template.contains("%s"));
        let addrs = read_chrome_addresses(&path).unwrap();
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].name, "Ada L");
        assert_eq!(addrs[0].city, "Dessau");
        assert_eq!(addrs[0].email, "ada@test.example");
        // Wrong file for each reader errors honestly.
        assert!(read_chrome_engines(&write_temp("ferrite_test_wd_bad.db", b"junk")).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn chrome_addresses_join_emails_and_skip_empty() {
        let path = make_sqlite(
            "ferrite_test_webdata2.db",
            &[
                "CREATE TABLE autofill_profiles(guid TEXT, full_name TEXT, first_name TEXT, last_name TEXT, street_address TEXT, city TEXT, state TEXT, zipcode TEXT, country_code TEXT, phone_home_whole_number TEXT);
                 CREATE TABLE autofill_profile_emails(profile_guid TEXT, email TEXT);
                 INSERT INTO autofill_profiles(guid, full_name, first_name, last_name, street_address, city, state, zipcode, country_code, phone_home_whole_number) VALUES('g1', '', 'Grace', 'Hopper', '', '', '', '', '', '');
                 INSERT INTO autofill_profile_emails(profile_guid, email) VALUES('g1', 'grace@test.example');
                 INSERT INTO autofill_profiles(guid) VALUES('g2');",
            ],
        );
        let addrs = read_chrome_addresses(&path).unwrap();
        assert_eq!(addrs.len(), 1);
        assert_eq!(addrs[0].name, "Grace Hopper");
        assert_eq!(addrs[0].email, "grace@test.example");
        let _ = std::fs::remove_file(path);
    }
}
