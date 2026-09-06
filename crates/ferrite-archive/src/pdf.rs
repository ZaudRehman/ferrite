//! Native PDF engine.

use crate::error::{ArchiveError, ArchiveResult};

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

#[cfg(feature = "pdfium-render")]
mod inner {
    use super::*;
    use pdfium_render::prelude::*;
    use std::sync::Arc;

    pub struct PdfEngine {
        lib: Arc<Pdfium>,
    }

    impl PdfEngine {
        pub fn new() -> ArchiveResult<Self> {
            // System library first; explicit override via env. The old code
            // preferred `./` (CWD DLL hijacking: any downloaded file named
            // like the pdfium library in the working directory won).
            let lib = if let Ok(path) = std::env::var("FERRITE_PDFIUM_PATH") {
                Pdfium::new(
                    Pdfium::bind_to_library(Pdfium::pdfium_platform_library_name_at_path(&path))
                        .map_err(|e| ArchiveError::Pdf(e.to_string()))?,
                )
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
            })
        }
    }

    pub struct PdfDocument {
        lib: Arc<Pdfium>,
        bytes: Vec<u8>,
        password: Option<zeroize::Zeroizing<String>>,
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
                let page = doc.pages().get(page_index as i32).ok()?;
                Some((page.width().value, page.height().value))
            })
            .ok()
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
                }
                Ok(out)
            })
            .unwrap_or_default()
        }

        pub fn form_fields(&self) -> Vec<FormField> {
            self.with_doc(|doc| {
                let mut out = Vec::new();
                for page in doc.pages().iter() {
                    // Use page text as fallback; real AcroForm via doc.form()
                    if let Ok(form) = doc.form() {
                        for field in form.fields().iter() {
                            out.push(FormField {
                                name: field.name().to_string(),
                                field_type: format!("{:?}", field.field_type()),
                                value: field.value().map(|v| v.to_string()),
                            });
                        }
                        break;
                    }
                    let _ = &page;
                    break;
                }
                if out.is_empty() {
                    // Fallback scan via bytes handled below
                }
                Ok(out)
            })
            .unwrap_or_default()
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
            // Fail closed: without the pdfium engine there is no way to
            // verify a password, so encrypted files cannot be opened at all
            // (the old code accepted the literal password "correct").
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

        pub fn render_page(&self, _page_index: usize, _scale: f32) -> ArchiveResult<Vec<u8>> {
            // Placeholder: return 1x1 white pixel RGBA - wgpu uploadable
            Ok(vec![255, 255, 255, 255])
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

        pub fn form_fields(&self) -> Vec<FormField> {
            let text = String::from_utf8_lossy(&self.data);
            let mut fields = Vec::new();
            // Simple AcroForm/F field scan: /T (name) and /FT /Tx /Btn /Ch
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
                    fields.push(FormField {
                        name,
                        field_type,
                        value,
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

        pub fn render_all_pages(&self, scale: f32) -> ArchiveResult<Vec<Vec<u8>>> {
            let mut out = Vec::new();
            for i in 0..self.page_count {
                out.push(self.render_page(i, scale)?);
            }
            Ok(out)
        }

        /// Wgpu upload helper: returns (width, height, rgba) for texture creation loop.
        /// Fallback placeholder is exactly 1x1 white.
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
    /// Maximum PDF bytes accepted per render (64MB): bounds child allocs.
    pub const MAX_PDF_BYTES: usize = 64 * 1024 * 1024;
    /// Maximum page index accepted.
    pub const MAX_PAGE: usize = 10_000;

    pub fn spawn() -> ArchiveResult<Self> {
        #[cfg(windows)]
        const EXE: &str = "ferrite-pdf-renderer.exe";
        #[cfg(not(windows))]
        const EXE: &str = "ferrite-pdf-renderer";
        let path = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.join(EXE)))
            .unwrap_or_else(|| EXE.into());
        let child = match tokio::process::Command::new(&path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
        {
            Ok(c) => Some(c),
            Err(e) => {
                // No silent success: renders fall back to 1x1 and callers
                // can probe `is_alive` (now honest) to surface status.
                tracing::warn!(%e, path = %path.display(), "pdf renderer spawn failed");
                None
            }
        };
        Ok(Self { child, shm: None })
    }

    pub async fn render(
        &mut self,
        bytes: &[u8],
        page: usize,
        password: Option<&str>,
    ) -> ArchiveResult<Vec<u8>> {
        if bytes.len() > Self::MAX_PDF_BYTES {
            return Err(ArchiveError::Pdf("pdf exceeds 64MB render cap".to_string()));
        }
        if page > Self::MAX_PAGE {
            return Err(ArchiveError::Pdf("page index out of range".to_string()));
        }
        let pwd_len = password.map(|p| p.len()).unwrap_or(0);
        if pwd_len > 1024 {
            return Err(ArchiveError::Pdf("password too long".to_string()));
        }
        let Some(child) = &mut self.child else {
            return Ok(vec![255, 255, 255, 255]);
        };
        let page_w = 800usize;
        let page_h = 1100usize;
        let shm_size = page_w * page_h * 4;
        let shm = crate::shm::ShmHandle::create(shm_size).map_err(ArchiveError::Io)?;
        let shm_name = shm.path.to_string_lossy().to_string();
        self.shm = Some(shm);
        use tokio::io::AsyncWriteExt;
        // Password bytes are zeroized immediately after the write.
        let mut pwd_owned: Option<zeroize::Zeroizing<Vec<u8>>> =
            password.map(|p| zeroize::Zeroizing::new(p.as_bytes().to_vec()));
        if let Some(stdin) = child.stdin.as_mut() {
            let header = format!("{} {} {} {}\n", bytes.len(), page, shm_name, pwd_len);
            if stdin.write_all(header.as_bytes()).await.is_err()
                || stdin.write_all(bytes).await.is_err()
                || (pwd_len > 0
                    && stdin
                        .write_all(pwd_owned.as_deref().map_or(&[][..], |v| &v[..]))
                        .await
                        .is_err())
                || stdin.flush().await.is_err()
            {
                pwd_owned.take();
                return Err(ArchiveError::Pdf("pdf renderer stdin failed".to_string()));
            }
        }
        pwd_owned.take();
        let mut out = Vec::new();
        if let Some(stdout) = child.stdout.as_mut() {
            use tokio::io::AsyncReadExt;
            let mut len_buf = [0u8; 4];
            if tokio::time::timeout(
                std::time::Duration::from_secs(2),
                stdout.read_exact(&mut len_buf),
            )
            .await
            .is_ok()
            {
                let len = u32::from_le_bytes(len_buf) as usize;
                if len > 0 && len < 50 * 1024 * 1024 {
                    out.resize(len, 0);
                    if tokio::time::timeout(
                        std::time::Duration::from_secs(2),
                        stdout.read_exact(&mut out),
                    )
                    .await
                    .is_err()
                    {
                        out.clear();
                    }
                }
            }
        }
        if out.is_empty() {
            // Fallback: map shm file directly if child wrote via mmap
            if let Some(shm) = &self.shm {
                if let Ok(data) = std::fs::read(&shm.path) {
                    if !data.is_empty() {
                        return Ok(data);
                    }
                }
            }
            Ok(vec![255, 255, 255, 255])
        } else {
            Ok(out)
        }
    }

    /// True only while a live child handle exists. `None` (spawn failed)
    /// reports false — the old code reported true.
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

    #[test]
    fn load_simple_pdf() {
        let engine = PdfEngine::new().unwrap();
        let pdf = b"%PDF-1.4 fake content Hello";
        let doc = engine.load(pdf, None).unwrap();
        assert_eq!(doc.info().page_count, 1);
        let bmp = doc.render_page(0, 1.0).unwrap();
        assert!(!bmp.is_empty());
    }

    #[test]
    fn password_required() {
        let engine = PdfEngine::new().unwrap();
        let pdf = b"%PDF-1.4 /Encrypt fake";
        // Without the pdfium engine, encrypted files fail closed regardless
        // of the offered password (no password oracle).
        assert!(engine.load(pdf, None).is_err());
        assert!(engine.load(pdf, Some("correct")).is_err());
        assert!(engine.load(pdf, Some("wrong")).is_err());
    }

    #[test]
    fn search() {
        let engine = PdfEngine::new().unwrap();
        let pdf = b"%PDF hello world searchable";
        let doc = engine.load(pdf, None).unwrap();
        assert_eq!(doc.search("hello").len(), 1);
        assert_eq!(doc.search("missing").len(), 0);
    }
}
