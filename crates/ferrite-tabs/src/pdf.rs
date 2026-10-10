//! PDF navigation classification, shared by the engine link intercept
//! ([`crate::webview`]) and the shell omnibox. Matching is deliberately
//! shallow (suffix on the path, case-insensitive): the `%PDF-` magic
//! check at open time is the real gate, so a lying suffix fails closed
//! instead of rendering.

/// What a navigation URL wants to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PdfNav {
    /// Already the viewer: let it through untouched.
    Viewer,
    /// Remote document: fetch, then view.
    Remote,
    /// Local file: read, then view.
    Local,
    /// Not a PDF navigation.
    No,
}

/// Classify a navigation URL.
pub fn classify_pdf_url(url: &str) -> PdfNav {
    if url.starts_with("ferrite://pdf/") {
        return PdfNav::Viewer;
    }
    let lower = url.to_ascii_lowercase();
    if url.strip_prefix("file://").is_some() {
        let rest = url
            .strip_prefix("file://localhost/")
            .or_else(|| url.strip_prefix("file://"))
            .unwrap_or(url);
        let path = rest.split(['?', '#']).next().unwrap_or("");
        if path.to_ascii_lowercase().ends_with(".pdf") {
            return PdfNav::Local;
        }
        return PdfNav::No;
    }
    if lower.starts_with("http://") || lower.starts_with("https://") {
        // Strip scheme + authority, match the path suffix only: a query
        // like `?x=.pdf` on an HTML page must not classify.
        let path = url.split("://").nth(1).unwrap_or("");
        let path = path.split(['?', '#']).next().unwrap_or("");
        // The first segment is the authority; a bare host never matches.
        if path.split('/').count() > 1 && path.to_ascii_lowercase().ends_with(".pdf") {
            return PdfNav::Remote;
        }
    }
    PdfNav::No
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_passes_through() {
        assert_eq!(classify_pdf_url("ferrite://pdf/?doc=abc"), PdfNav::Viewer);
    }

    #[test]
    fn remote_needs_path_suffix() {
        assert_eq!(
            classify_pdf_url("https://example.com/report.pdf"),
            PdfNav::Remote
        );
        assert_eq!(
            classify_pdf_url("https://example.com/report.PDF?dl=1#page=2"),
            PdfNav::Remote
        );
        assert_eq!(classify_pdf_url("https://example.com/?x=.pdf"), PdfNav::No);
        assert_eq!(classify_pdf_url("https://example.com/"), PdfNav::No);
    }

    #[test]
    fn local_files() {
        assert_eq!(classify_pdf_url("file:///C:/docs/a.pdf"), PdfNav::Local);
        assert_eq!(
            classify_pdf_url("file://localhost/home/u/a.PDF"),
            PdfNav::Local
        );
        assert_eq!(classify_pdf_url("file:///etc/passwd"), PdfNav::No);
    }

    #[test]
    fn weird_schemes_rejected() {
        assert_eq!(classify_pdf_url("javascript:alert(.pdf)"), PdfNav::No);
        assert_eq!(classify_pdf_url("data:application/pdf,foo"), PdfNav::No);
        assert_eq!(classify_pdf_url("ftp://h/x.pdf"), PdfNav::No);
    }
}
