//! File attachment subsystem (`/attach` palette, rules, PDF/image preparation,
//! directory browsing, clipboard image/URI capture, and attachment system prompts).

use crate::anim::{Spring, SpringSpec};
use crate::base64;
use crate::config::ApiFormat;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::Sender;
use std::time::{SystemTime, UNIX_EPOCH};

pub mod rules {
    pub const MAX_ATTACHMENTS: usize = 10;
    pub const MAX_IMAGES: usize = 4;
    pub const MAX_PDFS: usize = 4;
    pub const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
    pub const MAX_PDF_BYTES: u64 = 32 * 1024 * 1024;
    pub const MAX_PDF_NATIVE_BYTES: u64 = 20 * 1024 * 1024;
    pub const MAX_TEXT_BYTES: u64 = 4 * 1024 * 1024;
    pub const MAX_EXTRACTED_CHARS: usize = 60_000;
    pub const MAX_SCANNED_PDF_PAGES: usize = 4;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentKind {
    Image,
    Pdf,
    TextFile,
}

impl AttachmentKind {
    pub fn badge(self) -> &'static str {
        match self {
            Self::Image => "IMG",
            Self::Pdf => "PDF",
            Self::TextFile => "FILE",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttachmentStatus {
    Loading,
    Ready,
    Error(String),
}

#[derive(Debug, Clone)]
pub struct PreparedAttachment {
    pub kind: AttachmentKind,
    pub mime: String,
    pub size_bytes: u64,
    pub extracted_text: Option<String>,
    pub page_count: Option<usize>,
    pub truncated: bool,
    pub base64_data: Option<String>,
    pub page_images: Vec<(String, String)>,
    pub dimensions: Option<(u32, u32)>,
    pub note: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Attachment {
    pub id: u64,
    pub path: PathBuf,
    pub name: String,
    pub mime: String,
    pub kind: AttachmentKind,
    pub size_bytes: u64,
    pub status: AttachmentStatus,
    pub extracted_text: Option<String>,
    pub page_count: Option<usize>,
    pub truncated: bool,
    pub base64_data: Option<String>,
    pub page_images: Vec<(String, String)>,
    pub dimensions: Option<(u32, u32)>,
    pub note: Option<String>,
    pub entrance: Spring,
}

impl Attachment {
    pub fn new_loading(id: u64, path: PathBuf, kind: AttachmentKind, size_bytes: u64) -> Self {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(|s| s.to_string())
            .unwrap_or_else(|| path.display().to_string());
        let mime = guess_mime(&path);
        let mut entrance = Spring::new(0.0, SpringSpec::FAST_SPATIAL);
        entrance.retarget(1.0);
        Self {
            id,
            path,
            name,
            mime,
            kind,
            size_bytes,
            status: AttachmentStatus::Loading,
            extracted_text: None,
            page_count: None,
            truncated: false,
            base64_data: None,
            page_images: Vec::new(),
            dimensions: None,
            note: None,
            entrance,
        }
    }

    pub fn from_prepared(id: u64, path: PathBuf, prep: PreparedAttachment) -> Self {
        let mut att = Self::new_loading(id, path, prep.kind, prep.size_bytes);
        att.apply_prepared(prep);
        att.entrance.snap_to(1.0);
        att
    }

    pub fn apply_prepared(&mut self, prep: PreparedAttachment) {
        self.kind = prep.kind;
        self.mime = prep.mime;
        self.size_bytes = prep.size_bytes;
        self.extracted_text = prep.extracted_text;
        self.page_count = prep.page_count;
        self.truncated = prep.truncated;
        self.base64_data = prep.base64_data;
        self.page_images = prep.page_images;
        self.dimensions = prep.dimensions;
        self.note = prep.note;
        self.status = match prep.error {
            Some(err) => AttachmentStatus::Error(err),
            None => AttachmentStatus::Ready,
        };
    }

    pub fn chip_subtitle(&self) -> String {
        match &self.status {
            AttachmentStatus::Loading => "loading…".to_string(),
            AttachmentStatus::Error(_) => "error".to_string(),
            AttachmentStatus::Ready => match self.kind {
                AttachmentKind::Image => {
                    if let Some((w, h)) = self.dimensions {
                        format!("{}×{}", w, h)
                    } else {
                        format_bytes(self.size_bytes)
                    }
                }
                AttachmentKind::Pdf => {
                    if let Some(p) = self.page_count {
                        format!("{}p", p)
                    } else {
                        format_bytes(self.size_bytes)
                    }
                }
                AttachmentKind::TextFile => format_bytes(self.size_bytes),
            },
        }
    }

    pub fn to_payload(&self) -> AttachmentPayload {
        AttachmentPayload {
            name: self.name.clone(),
            path: self.path.display().to_string(),
            mime: self.mime.clone(),
            kind: self.kind,
            size_bytes: self.size_bytes,
            extracted_text: self.extracted_text.clone(),
            page_count: self.page_count,
            truncated: self.truncated,
            base64_data: self.base64_data.clone(),
            page_images: self.page_images.clone(),
            dimensions: self.dimensions,
            note: self.note.clone(),
        }
    }
}

/// Immutable attachment data attached to a sent user turn.
#[derive(Debug, Clone)]
pub struct AttachmentPayload {
    pub name: String,
    pub path: String,
    pub mime: String,
    pub kind: AttachmentKind,
    pub size_bytes: u64,
    pub extracted_text: Option<String>,
    pub page_count: Option<usize>,
    pub truncated: bool,
    pub base64_data: Option<String>,
    pub page_images: Vec<(String, String)>,
    pub dimensions: Option<(u32, u32)>,
    pub note: Option<String>,
}

impl AttachmentPayload {
    pub fn chip_label(&self) -> String {
        match self.kind {
            AttachmentKind::Image => {
                if let Some((w, h)) = self.dimensions {
                    format!("{} · {}×{}", self.name, w, h)
                } else {
                    self.name.clone()
                }
            }
            AttachmentKind::Pdf => {
                if let Some(p) = self.page_count {
                    format!("{} · {}p", self.name, p)
                } else {
                    self.name.clone()
                }
            }
            AttachmentKind::TextFile => self.name.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub enum AttachEvent {
    Prepared {
        id: u64,
        result: PreparedAttachment,
    },
    ClipboardResult {
        result: Result<PathBuf, String>,
    },
}

#[derive(Debug, Clone)]
pub struct AttachDirEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub kind: Option<AttachmentKind>,
    pub size_bytes: u64,
}

pub fn classify_path(path: &Path) -> AttachmentKind {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" => AttachmentKind::Image,
        "pdf" => AttachmentKind::Pdf,
        _ => AttachmentKind::TextFile,
    }
}

pub fn guess_mime(path: &Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "md" | "markdown" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        _ => "text/plain",
    }
    .to_string()
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{} kB", (bytes + 512) / 1024)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}

pub fn validate_before_attach(
    existing: &[Attachment],
    path: &Path,
) -> Result<(AttachmentKind, u64), String> {
    if existing.len() >= rules::MAX_ATTACHMENTS {
        return Err(format!(
            "Attachment limit reached (max {} files per message)",
            rules::MAX_ATTACHMENTS
        ));
    }
    let meta = fs::metadata(path).map_err(|e| format!("Cannot read {}: {}", path.display(), e))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    let canon = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if existing
        .iter()
        .any(|a| fs::canonicalize(&a.path).unwrap_or_else(|_| a.path.clone()) == canon)
    {
        return Err(format!(
            "{} is already attached",
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
        ));
    }

    let kind = classify_path(path);
    let size = meta.len();
    match kind {
        AttachmentKind::Image => {
            let count = existing
                .iter()
                .filter(|a| a.kind == AttachmentKind::Image)
                .count();
            if count >= rules::MAX_IMAGES {
                return Err(format!(
                    "Image limit reached (max {} images per message)",
                    rules::MAX_IMAGES
                ));
            }
            if size > rules::MAX_IMAGE_BYTES {
                return Err(format!(
                    "Image exceeds {} limit ({})",
                    format_bytes(rules::MAX_IMAGE_BYTES),
                    format_bytes(size)
                ));
            }
        }
        AttachmentKind::Pdf => {
            let count = existing
                .iter()
                .filter(|a| a.kind == AttachmentKind::Pdf)
                .count();
            if count >= rules::MAX_PDFS {
                return Err(format!(
                    "PDF limit reached (max {} PDFs per message)",
                    rules::MAX_PDFS
                ));
            }
            if size > rules::MAX_PDF_BYTES {
                return Err(format!(
                    "PDF exceeds {} limit ({})",
                    format_bytes(rules::MAX_PDF_BYTES),
                    format_bytes(size)
                ));
            }
        }
        AttachmentKind::TextFile => {
            if size > rules::MAX_TEXT_BYTES {
                return Err(format!(
                    "File exceeds {} limit ({})",
                    format_bytes(rules::MAX_TEXT_BYTES),
                    format_bytes(size)
                ));
            }
        }
    }
    Ok((kind, size))
}

pub fn prepare_attachment_async(id: u64, path: PathBuf, tx: Sender<AttachEvent>) {
    std::thread::spawn(move || {
        let result = prepare_attachment_blocking(&path);
        let _ = tx.send(AttachEvent::Prepared { id, result });
    });
}

pub fn prepare_attachment_blocking(path: &Path) -> PreparedAttachment {
    let kind = classify_path(path);
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return PreparedAttachment {
                kind,
                mime: guess_mime(path),
                size_bytes: 0,
                extracted_text: None,
                page_count: None,
                truncated: false,
                base64_data: None,
                page_images: Vec::new(),
                dimensions: None,
                note: None,
                error: Some(format!("Failed to read file: {}", e)),
            };
        }
    };
    let size_bytes = bytes.len() as u64;

    match kind {
        AttachmentKind::Image => prepare_image(path, &bytes, size_bytes),
        AttachmentKind::Pdf => prepare_pdf(path, &bytes, size_bytes),
        AttachmentKind::TextFile => prepare_text_file(path, &bytes, size_bytes),
    }
}

fn prepare_image(path: &Path, bytes: &[u8], size_bytes: u64) -> PreparedAttachment {
    let (mime, dimensions) = inspect_image_header(bytes, path);
    let note = if mime == "image/bmp" {
        Some("BMP format may be rejected by some vision APIs; PNG/JPEG/WebP preferred".to_string())
    } else {
        None
    };
    let encoded = base64::encode(bytes);
    PreparedAttachment {
        kind: AttachmentKind::Image,
        mime,
        size_bytes,
        extracted_text: None,
        page_count: None,
        truncated: false,
        base64_data: Some(encoded),
        page_images: Vec::new(),
        dimensions,
        note,
        error: None,
    }
}

fn prepare_pdf(path: &Path, bytes: &[u8], size_bytes: u64) -> PreparedAttachment {
    let raw_b64 = if size_bytes <= rules::MAX_PDF_NATIVE_BYTES {
        Some(base64::encode(bytes))
    } else {
        None
    };

    let (raw_text, mut page_count, method_note) = extract_pdf_text_cascade(path, bytes);
    let non_ws_chars = raw_text.chars().filter(|c| !c.is_whitespace()).count();

    let mut page_images = Vec::new();
    let mut note = method_note;

    if non_ws_chars < 32 {
        // Likely a scanned or image-only PDF: attempt rendering first pages for vision models.
        page_images = render_pdf_pages_png(path, rules::MAX_SCANNED_PDF_PAGES);
        if !page_images.is_empty() {
            if page_count.is_none() {
                page_count = Some(page_images.len());
            }
            note = Some(format!(
                "scanned PDF · {} page image(s) rendered",
                page_images.len()
            ));
        } else if non_ws_chars == 0 {
            note = Some("no extractable text (scanned or encrypted PDF)".to_string());
        }
    }

    let (extracted_text, truncated) = if raw_text.trim().is_empty() {
        (None, false)
    } else {
        let (clipped, was_trunc) = truncate_chars_boundary(&raw_text, rules::MAX_EXTRACTED_CHARS);
        (Some(clipped), was_trunc)
    };

    PreparedAttachment {
        kind: AttachmentKind::Pdf,
        mime: "application/pdf".to_string(),
        size_bytes,
        extracted_text,
        page_count,
        truncated,
        base64_data: raw_b64,
        page_images,
        dimensions: None,
        note,
        error: None,
    }
}

fn prepare_text_file(path: &Path, bytes: &[u8], size_bytes: u64) -> PreparedAttachment {
    let probe = &bytes[..bytes.len().min(8192)];
    if probe.contains(&0) {
        return PreparedAttachment {
            kind: AttachmentKind::TextFile,
            mime: "application/octet-stream".to_string(),
            size_bytes,
            extracted_text: None,
            page_count: None,
            truncated: false,
            base64_data: None,
            page_images: Vec::new(),
            dimensions: None,
            note: None,
            error: Some("Binary file cannot be attached as text".to_string()),
        };
    }

    let full = String::from_utf8_lossy(bytes);
    let (clipped, truncated) = truncate_chars_boundary(&full, rules::MAX_EXTRACTED_CHARS);
    PreparedAttachment {
        kind: AttachmentKind::TextFile,
        mime: guess_mime(path),
        size_bytes,
        extracted_text: Some(clipped),
        page_count: None,
        truncated,
        base64_data: None,
        page_images: Vec::new(),
        dimensions: None,
        note: None,
        error: None,
    }
}

fn truncate_chars_boundary(s: &str, max_bytes_approx: usize) -> (String, bool) {
    if s.len() <= max_bytes_approx {
        return (s.to_string(), false);
    }
    let mut end = max_bytes_approx;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let omitted = s.len().saturating_sub(end);
    let mut out = s[..end].to_string();
    out.push_str(&format!(
        "\n… (truncated {} bytes at attachment budget limit)",
        omitted
    ));
    (out, true)
}

fn extract_pdf_text_cascade(path: &Path, raw_bytes: &[u8]) -> (String, Option<usize>, Option<String>) {
    // 1. `pdftotext` (poppler-utils) — preserves layout & splits pages with form-feed `\x0c`.
    if let Ok(out) = Command::new("pdftotext")
        .args(["-layout", "-enc", "UTF-8"])
        .arg(path)
        .arg("-")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout);
            let (formatted, pages) = format_formfeed_pages(&raw);
            return (formatted, Some(pages), Some("pdftotext".to_string()));
        }
    }

    // 2. `mutool draw -F txt`
    if let Ok(out) = Command::new("mutool")
        .args(["draw", "-F", "txt", "-o", "-"])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout);
            let (formatted, pages) = format_formfeed_pages(&raw);
            return (formatted, Some(pages), Some("mutool".to_string()));
        }
    }

    // 3. `python3` with pypdf / fitz / pdfplumber if installed
    let py_script = r#"
import sys
p = sys.argv[1]
try:
    import pypdf
    r = pypdf.PdfReader(p)
    print("\x0c".join((pg.extract_text() or "") for pg in r.pages))
    sys.exit(0)
except Exception:
    pass
try:
    import fitz
    d = fitz.open(p)
    print("\x0c".join(pg.get_text() for pg in d))
    sys.exit(0)
except Exception:
    pass
try:
    import pdfplumber
    with pdfplumber.open(p) as d:
        print("\x0c".join((pg.extract_text() or "") for pg in d.pages))
    sys.exit(0)
except Exception:
    pass
sys.exit(1)
"#;
    if let Ok(out) = Command::new("python3")
        .args(["-c", py_script])
        .arg(path)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            let raw = String::from_utf8_lossy(&out.stdout);
            let (formatted, pages) = format_formfeed_pages(&raw);
            return (formatted, Some(pages), Some("python-pdf".to_string()));
        }
    }

    // 4. Pure-Rust lightweight fallback for uncompressed PDF text operators + page counting.
    let (txt, pages) = extract_pdf_text_rust_fallback(raw_bytes);
    (txt, pages, Some("built-in pdf scan".to_string()))
}

fn format_formfeed_pages(raw: &str) -> (String, usize) {
    let mut raw_pages: Vec<&str> = raw.split('\x0c').collect();
    if raw_pages.len() > 1 && raw_pages.last().map_or(false, |s| s.trim().is_empty()) {
        raw_pages.pop();
    }
    let page_count = raw_pages.len().max(1);
    let mut out = String::new();
    for (idx, body) in raw_pages.iter().enumerate() {
        let trimmed = body.trim();
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&format!("[page {}]\n{}", idx + 1, trimmed));
    }
    (out, page_count)
}

fn extract_pdf_text_rust_fallback(bytes: &[u8]) -> (String, Option<usize>) {
    // Count `/Type /Page` (excluding `/Type /Pages`) as rough page count.
    let mut page_hits = 0usize;
    let hay = String::from_utf8_lossy(bytes);
    for (pos, _) in hay.match_indices("/Type") {
        let after = hay[pos + 5..].trim_start();
        if after.starts_with("/Page") && !after.starts_with("/Pages") {
            page_hits += 1;
        }
    }
    let page_count = if page_hits > 0 { Some(page_hits) } else { None };

    // Extract literal strings inside `( ... )` preceding `Tj` / `TJ` in uncompressed streams.
    let mut collected = String::new();
    let mut i = 0usize;
    while i < bytes.len() && collected.len() < rules::MAX_EXTRACTED_CHARS {
        if bytes[i] == b'(' {
            i += 1;
            let mut lit = String::new();
            let mut depth = 1usize;
            while i < bytes.len() && depth > 0 {
                match bytes[i] {
                    b'\\' if i + 1 < bytes.len() => {
                        i += 1;
                        match bytes[i] {
                            b'n' => lit.push('\n'),
                            b'r' => lit.push('\r'),
                            b't' => lit.push('\t'),
                            b'(' => lit.push('('),
                            b')' => lit.push(')'),
                            b'\\' => lit.push('\\'),
                            other if other.is_ascii_graphic() => lit.push(other as char),
                            _ => {}
                        }
                    }
                    b'(' => {
                        depth += 1;
                        lit.push('(');
                    }
                    b')' => {
                        depth -= 1;
                        if depth > 0 {
                            lit.push(')');
                        }
                    }
                    b => {
                        if b.is_ascii_graphic() || b == b' ' {
                            lit.push(b as char);
                        }
                    }
                }
                i += 1;
            }
            // Peek ahead for Tj / TJ / ' / "
            let peek_end = (i + 16).min(bytes.len());
            let after = std::str::from_utf8(&bytes[i..peek_end]).unwrap_or("");
            let is_text_op = after.trim_start().starts_with("Tj")
                || after.trim_start().starts_with("TJ")
                || after.contains("]TJ")
                || after.contains("] TJ");
            if is_text_op && !lit.trim().is_empty() {
                if !collected.is_empty() && !collected.ends_with(['\n', ' ']) {
                    collected.push(' ');
                }
                collected.push_str(lit.trim());
            }
        } else {
            i += 1;
        }
    }

    if collected.trim().is_empty() {
        (String::new(), page_count)
    } else {
        (format!("[page 1]\n{}", collected), page_count)
    }
}

fn render_pdf_pages_png(path: &Path, max_pages: usize) -> Vec<(String, String)> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp_dir = std::env::temp_dir().join(format!("thetalk-pdf-{}", stamp));
    if fs::create_dir_all(&tmp_dir).is_err() {
        return Vec::new();
    }
    let prefix = tmp_dir.join("page");
    let ok = Command::new("pdftoppm")
        .args(["-png", "-r", "130", "-f", "1", "-l"])
        .arg(max_pages.to_string())
        .arg(path)
        .arg(&prefix)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);

    let mut out = Vec::new();
    if ok {
        if let Ok(entries) = fs::read_dir(&tmp_dir) {
            let mut pngs: Vec<PathBuf> = entries
                .filter_map(|e| e.ok().map(|x| x.path()))
                .filter(|p| {
                    p.extension()
                        .and_then(|s| s.to_str())
                        .map(|s| s.eq_ignore_ascii_case("png"))
                        .unwrap_or(false)
                })
                .collect();
            pngs.sort();
            for p in pngs.into_iter().take(max_pages) {
                if let Ok(bytes) = fs::read(&p) {
                    out.push(("image/png".to_string(), base64::encode(&bytes)));
                }
            }
        }
    }
    let _ = fs::remove_dir_all(&tmp_dir);
    out
}

fn inspect_image_header(bytes: &[u8], path: &Path) -> (String, Option<(u32, u32)>) {
    // PNG: signature + IHDR chunk
    if bytes.len() >= 24 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" {
        let w = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
        let h = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
        return ("image/png".to_string(), Some((w, h)));
    }
    // GIF
    if bytes.len() >= 10 && (&bytes[0..6] == b"GIF87a" || &bytes[0..6] == b"GIF89a") {
        let w = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;
        let h = u16::from_le_bytes([bytes[8], bytes[9]]) as u32;
        return ("image/gif".to_string(), Some((w, h)));
    }
    // BMP
    if bytes.len() >= 26 && &bytes[0..2] == b"BM" {
        let w = i32::from_le_bytes([bytes[18], bytes[19], bytes[20], bytes[21]]).unsigned_abs();
        let h = i32::from_le_bytes([bytes[22], bytes[23], bytes[24], bytes[25]]).unsigned_abs();
        return ("image/bmp".to_string(), Some((w, h)));
    }
    // WebP
    if bytes.len() >= 30 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        let dims = if &bytes[12..16] == b"VP8X" && bytes.len() >= 30 {
            let w = 1 + (bytes[24] as u32 | ((bytes[25] as u32) << 8) | ((bytes[26] as u32) << 16));
            let h = 1 + (bytes[27] as u32 | ((bytes[28] as u32) << 8) | ((bytes[29] as u32) << 16));
            Some((w, h))
        } else if &bytes[12..16] == b"VP8 " && bytes.len() >= 30 {
            let w = u16::from_le_bytes([bytes[26], bytes[27]]) as u32 & 0x3FFF;
            let h = u16::from_le_bytes([bytes[28], bytes[29]]) as u32 & 0x3FFF;
            Some((w, h))
        } else {
            None
        };
        return ("image/webp".to_string(), dims);
    }
    // JPEG: scan SOF0..SOF3 markers
    if bytes.len() >= 4 && bytes[0] == 0xFF && bytes[1] == 0xD8 {
        let mut i = 2usize;
        let mut dims = None;
        while i + 9 < bytes.len() {
            if bytes[i] != 0xFF {
                i += 1;
                continue;
            }
            while i < bytes.len() && bytes[i] == 0xFF {
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            let marker = bytes[i];
            i += 1;
            if marker == 0xD9 || marker == 0xDA {
                break;
            }
            if marker == 0x00 || (0xD0..=0xD7).contains(&marker) {
                continue;
            }
            if i + 2 > bytes.len() {
                break;
            }
            let seg_len = u16::from_be_bytes([bytes[i], bytes[i + 1]]) as usize;
            if seg_len < 2 || i + seg_len > bytes.len() {
                break;
            }
            if matches!(marker, 0xC0 | 0xC1 | 0xC2 | 0xC3) && seg_len >= 7 {
                let h = u16::from_be_bytes([bytes[i + 3], bytes[i + 4]]) as u32;
                let w = u16::from_be_bytes([bytes[i + 5], bytes[i + 6]]) as u32;
                dims = Some((w, h));
                break;
            }
            i += seg_len;
        }
        return ("image/jpeg".to_string(), dims);
    }

    (guess_mime(path), None)
}

/// Builds a focused, grounded system prompt whenever the conversation includes
/// attached PDFs, images, or text files. Returns `None` for plain text-only chats.
pub fn build_attachment_system_prompt(
    attachments: &[&AttachmentPayload],
    vision_enabled: bool,
    api_format: ApiFormat,
) -> Option<String> {
    if attachments.is_empty() {
        return None;
    }
    let has_images = attachments.iter().any(|a| a.kind == AttachmentKind::Image);
    let has_pdfs = attachments.iter().any(|a| a.kind == AttachmentKind::Pdf);
    let has_files = attachments.iter().any(|a| a.kind == AttachmentKind::TextFile);

    let mut sections: Vec<String> = Vec::new();
    sections.push(
        "# Attachment Rules\n\
         The user has attached local file(s) to this conversation. Ground your answers strictly in the attached content."
            .to_string(),
    );

    if has_images {
        if vision_enabled {
            sections.push(
                "## Image Attachments (Vision Enabled)\n\
                 - Inspect the attached visual image content directly as primary evidence.\n\
                 - Transcribe visible text, numbers, error messages, code, UI labels, and chart values accurately.\n\
                 - If multiple images are attached, identify each by its filename.\n\
                 - Never guess or invent obscured, cropped, or illegible details; explicitly state what cannot be read clearly."
                    .to_string(),
            );
        } else {
            sections.push(
                "## Image Attachments (Vision Disabled)\n\
                 - Vision is OFF on the active model profile, so image pixels were NOT sent — only filenames/paths/dimensions are attached.\n\
                 - Do NOT pretend to see image contents or speculate about what an image depicts.\n\
                 - Clearly tell the user to turn Vision on in `/settings` or switch to a vision-capable profile to analyze images."
                    .to_string(),
            );
        }
    }

    if has_pdfs {
        let delivery = match api_format {
            ApiFormat::Anthropic => {
                "PDFs may arrive as native document attachments and/or `<attached_pdf>` extracted text with `[page N]` markers."
            }
            ApiFormat::OpenAi => {
                "PDFs are supplied in `<attached_pdf>` blocks with `[page N]` markers (and rendered page images when scanned)."
            }
        };
        sections.push(format!(
            "## PDF Attachments\n\
             - {}\n\
             - Always cite exact page numbers (e.g., `page 3` or `(p. 3)`) for facts, quotes, figures, and table entries.\n\
             - Preserve numerical precision, units, and table structure; do not silently round or alter extracted values.\n\
             - Never extrapolate beyond provided pages or fabricate truncated sections: if `truncated=\"true\"` or a page has no extractable text, state that explicitly.",
            delivery
        ));
    }

    if has_files {
        sections.push(
            "## File Attachments\n\
             - Text and source files are provided inside `<attached_file name=\"...\" path=\"...\">` blocks.\n\
             - Reference exact symbol names, keys, or lines from the file, and clearly distinguish observed contents from suggested edits."
                .to_string(),
        );
    }

    Some(sections.join("\n\n"))
}

/// Formats the text portion of a user message that includes attachments.
pub fn format_user_turn_text(raw_text: &str, attachments: &[AttachmentPayload], vision_enabled: bool) -> String {
    if attachments.is_empty() {
        return raw_text.to_string();
    }
    let mut out = String::new();
    let trimmed = raw_text.trim();
    if trimmed.is_empty() {
        out.push_str("Analyze the attached file(s).");
    } else {
        out.push_str(trimmed);
    }

    for att in attachments {
        match att.kind {
            AttachmentKind::Pdf => {
                let pages = att
                    .page_count
                    .map(|p| p.to_string())
                    .unwrap_or_else(|| "unknown".to_string());
                if let Some(txt) = &att.extracted_text {
                    out.push_str(&format!(
                        "\n\n<attached_pdf name=\"{}\" path=\"{}\" pages=\"{}\" truncated=\"{}\">\n{}\n</attached_pdf>",
                        escape_attr(&att.name),
                        escape_attr(&att.path),
                        pages,
                        att.truncated,
                        txt
                    ));
                } else {
                    let note = att.note.as_deref().unwrap_or("native/visual PDF");
                    out.push_str(&format!(
                        "\n\n<attached_pdf name=\"{}\" path=\"{}\" pages=\"{}\" note=\"{}\">(see attached document/page images)</attached_pdf>",
                        escape_attr(&att.name),
                        escape_attr(&att.path),
                        pages,
                        escape_attr(note)
                    ));
                }
            }
            AttachmentKind::TextFile => {
                if let Some(txt) = &att.extracted_text {
                    out.push_str(&format!(
                        "\n\n<attached_file name=\"{}\" path=\"{}\" truncated=\"{}\">\n{}\n</attached_file>",
                        escape_attr(&att.name),
                        escape_attr(&att.path),
                        att.truncated,
                        txt
                    ));
                }
            }
            AttachmentKind::Image => {
                let dims = att
                    .dimensions
                    .map(|(w, h)| format!("{}x{}", w, h))
                    .unwrap_or_else(|| format_bytes(att.size_bytes));
                if vision_enabled && att.base64_data.is_some() {
                    out.push_str(&format!(
                        "\n\n[attached image: {} ({}, {})]",
                        att.name, dims, att.path
                    ));
                } else {
                    out.push_str(&format!(
                        "\n\n[attached image: {} ({}, {}) — image pixels omitted because Vision is disabled]",
                        att.name, dims, att.path
                    ));
                }
            }
        }
    }
    out
}

fn escape_attr(s: &str) -> String {
    s.replace('"', "&quot;").replace('<', "&lt;").replace('>', "&gt;")
}

pub fn expand_user_path(raw: &str) -> PathBuf {
    let trimmed = raw.trim();
    if trimmed == "~" {
        return home_dir();
    }
    if let Some(rest) = trimmed.strip_prefix("~/") {
        return home_dir().join(rest);
    }
    PathBuf::from(trimmed)
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

pub fn quick_attach_dirs() -> Vec<(String, PathBuf)> {
    let home = home_dir();
    let cwd = std::env::current_dir().unwrap_or_else(|_| home.clone());
    let mut candidates = vec![
        ("Current folder (.)".to_string(), cwd),
        ("Home (~)".to_string(), home.clone()),
        ("Downloads".to_string(), home.join("Downloads")),
        ("Pictures".to_string(), home.join("Pictures")),
        ("Documents".to_string(), home.join("Documents")),
        ("Desktop".to_string(), home.join("Desktop")),
        ("/tmp".to_string(), PathBuf::from("/tmp")),
    ];
    candidates.retain(|(_, p)| p.is_dir());
    candidates
}

pub fn list_dir_entries(dir: &Path, filter: &str) -> Vec<AttachDirEntry> {
    let q = filter.trim().to_ascii_lowercase();
    let show_hidden = q.starts_with('.');
    let Ok(rd) = fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut entries = Vec::new();
    for item in rd.flatten() {
        let name = item.file_name().to_string_lossy().to_string();
        if !show_hidden && name.starts_with('.') {
            continue;
        }
        if !q.is_empty() && !name.to_ascii_lowercase().contains(&q) {
            continue;
        }
        let path = item.path();
        let meta = match item.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let is_dir = meta.is_dir();
        let kind = if is_dir {
            None
        } else {
            Some(classify_path(&path))
        };
        entries.push(AttachDirEntry {
            name,
            path,
            is_dir,
            kind,
            size_bytes: if is_dir { 0 } else { meta.len() },
        });
    }

    entries.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_ascii_lowercase().cmp(&b.name.to_ascii_lowercase()))
    });
    entries
}

/// Captures an image (`image/png`, `image/jpeg`, `image/webp`) or file URI/path from the
/// Wayland/X11 clipboard asynchronously and reports the local file path via `AttachEvent`.
pub fn paste_clipboard_attachment_async(tx: Sender<AttachEvent>) {
    std::thread::spawn(move || {
        let res = read_clipboard_attachment_blocking();
        let _ = tx.send(AttachEvent::ClipboardResult { result: res });
    });
}

fn read_clipboard_attachment_blocking() -> Result<PathBuf, String> {
    if let Ok(out) = Command::new("wl-paste")
        .arg("--list-types")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        if out.status.success() {
            let types = String::from_utf8_lossy(&out.stdout);
            for (mime, ext) in [
                ("image/png", "png"),
                ("image/jpeg", "jpg"),
                ("image/webp", "webp"),
            ] {
                if types.lines().any(|l| l.trim() == mime) {
                    if let Ok(img) = Command::new("wl-paste")
                        .args(["--type", mime])
                        .stdin(Stdio::null())
                        .stderr(Stdio::null())
                        .output()
                    {
                        if img.status.success() && !img.stdout.is_empty() {
                            return write_temp_clipboard_image(&img.stdout, ext);
                        }
                    }
                }
            }
            if types.lines().any(|l| l.trim() == "text/uri-list") {
                if let Ok(uris) = Command::new("wl-paste")
                    .args(["--type", "text/uri-list"])
                    .stdin(Stdio::null())
                    .stderr(Stdio::null())
                    .output()
                {
                    if let Some(p) = parse_uri_or_path(&String::from_utf8_lossy(&uris.stdout)) {
                        return Ok(p);
                    }
                }
            }
            if let Ok(txt) = Command::new("wl-paste")
                .arg("--no-newline")
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .output()
            {
                if txt.status.success() {
                    if let Some(p) = parse_uri_or_path(&String::from_utf8_lossy(&txt.stdout)) {
                        return Ok(p);
                    }
                }
            }
        }
    }

    // X11 fallback (`xclip`)
    if let Ok(img) = Command::new("xclip")
        .args(["-selection", "clipboard", "-t", "image/png", "-o"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
    {
        if img.status.success() && img.stdout.starts_with(b"\x89PNG") {
            return write_temp_clipboard_image(&img.stdout, "png");
        }
    }

    Err("No image or valid file path found on clipboard".to_string())
}

fn write_temp_clipboard_image(bytes: &[u8], ext: &str) -> Result<PathBuf, String> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("thetalk-clips");
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let path = dir.join(format!("clipboard-{}.{}", ts, ext));
    fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path)
}

pub fn parse_uri_or_path(raw: &str) -> Option<PathBuf> {
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let candidate = if let Some(rest) = trimmed.strip_prefix("file://") {
            percent_decode(rest)
        } else {
            trimmed.to_string()
        };
        let path = expand_user_path(&candidate);
        if path.is_file() {
            return Some(path);
        }
    }
    None
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2])) {
                out.push((h << 4) | l);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_system_prompt_rules_for_pdf_and_image_with_vision_gates() {
        let img = AttachmentPayload {
            name: "chart.png".into(),
            path: "/tmp/chart.png".into(),
            mime: "image/png".into(),
            kind: AttachmentKind::Image,
            size_bytes: 1024,
            extracted_text: None,
            page_count: None,
            truncated: false,
            base64_data: Some("AAAA".into()),
            page_images: Vec::new(),
            dimensions: Some((800, 600)),
            note: None,
        };
        let pdf = AttachmentPayload {
            name: "paper.pdf".into(),
            path: "/tmp/paper.pdf".into(),
            mime: "application/pdf".into(),
            kind: AttachmentKind::Pdf,
            size_bytes: 4096,
            extracted_text: Some("[page 1]\nIntroduction".into()),
            page_count: Some(5),
            truncated: false,
            base64_data: Some("JVBERi0=".into()),
            page_images: Vec::new(),
            dimensions: None,
            note: None,
        };

        assert!(build_attachment_system_prompt(&[], true, ApiFormat::OpenAi).is_none());

        let sys_vis_on = build_attachment_system_prompt(&[&img, &pdf], true, ApiFormat::OpenAi)
            .expect("system prompt expected");
        assert!(sys_vis_on.contains("Vision Enabled"));
        assert!(sys_vis_on.contains("cite exact page numbers"));
        assert!(sys_vis_on.contains("[page N]"));

        let sys_vis_off =
            build_attachment_system_prompt(&[&img], false, ApiFormat::OpenAi).unwrap();
        assert!(sys_vis_off.contains("Vision Disabled"));
        assert!(sys_vis_off.contains("Do NOT pretend to see image contents"));
    }

    #[test]
    fn formats_pdf_and_image_user_turn_text_cleanly() {
        let pdf = AttachmentPayload {
            name: "spec.pdf".into(),
            path: "/tmp/spec.pdf".into(),
            mime: "application/pdf".into(),
            kind: AttachmentKind::Pdf,
            size_bytes: 2000,
            extracted_text: Some("[page 1]\nHello PDF".into()),
            page_count: Some(1),
            truncated: false,
            base64_data: None,
            page_images: Vec::new(),
            dimensions: None,
            note: None,
        };
        let txt = format_user_turn_text("", &[pdf], true);
        assert!(txt.contains("Analyze the attached file(s)."));
        assert!(txt.contains("<attached_pdf name=\"spec.pdf\""));
        assert!(txt.contains("[page 1]\nHello PDF"));
    }

    #[test]
    fn enforces_attachment_validation_rules() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let tmp = std::env::temp_dir().join(format!("thetalk-attach-test-{}.png", stamp));
        // 1x1 PNG header
        let png: [u8; 24] = [
            0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
            0, 0, 0, 1, 0, 0, 0, 1,
        ];
        fs::write(&tmp, &png).unwrap();

        let (kind, _) = validate_before_attach(&[], &tmp).unwrap();
        assert_eq!(kind, AttachmentKind::Image);

        let prep = prepare_attachment_blocking(&tmp);
        assert_eq!(prep.mime, "image/png");
        assert_eq!(prep.dimensions, Some((1, 1)));
        assert!(prep.base64_data.is_some());

        let existing = vec![Attachment::from_prepared(1, tmp.clone(), prep)];
        // Duplicate prevention
        assert!(validate_before_attach(&existing, &tmp).is_err());

        let _ = fs::remove_file(&tmp);
    }

    #[test]
    fn parses_file_uris_with_percent_encoding() {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let p = std::env::temp_dir().join(format!("thetalk uri {}.txt", stamp));
        fs::write(&p, b"ok").unwrap();
        let uri = format!("file://{}", p.to_string_lossy().replace(' ', "%20"));
        assert_eq!(parse_uri_or_path(&uri).as_ref(), Some(&p));
        let _ = fs::remove_file(&p);
    }
}
