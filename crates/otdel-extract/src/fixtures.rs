//! Synthetic PDFs, built in memory.
//!
//! Enabled by the `fixtures` feature and used by the tests of this crate, of the worker
//! and of the API. No document is committed to the repository: the real partner
//! catalogues stay private (`AGENTS.md`), and a test that depended on them could not run
//! in CI anyway.
//!
//! The generated files are ordinary PDFs — catalog, page tree, a Type 1 base font,
//! content streams and a cross-reference table — so the reader under test does the same
//! work it does on a real document. Text is restricted to WinAnsi-representable
//! characters because these fixtures use a standard base font with no embedded encoding;
//! the Cyrillic path is exercised against the real catalogues by hand
//! (`docs/extraction-1b.md`).

/// One page under construction.
#[derive(Debug, Clone)]
pub struct PageBuilder {
    width: f64,
    height: f64,
    /// `(x, y, font size, text)` in PDF user space.
    texts: Vec<(f64, f64, f64, String)>,
    /// Draw the sample image XObject on this page.
    image: bool,
}

impl Default for PageBuilder {
    fn default() -> Self {
        // A4 in points.
        Self {
            width: 595.0,
            height: 842.0,
            texts: Vec::new(),
            image: false,
        }
    }
}

impl PageBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn landscape() -> Self {
        Self {
            width: 842.0,
            height: 595.0,
            ..Self::default()
        }
    }

    /// Place `text` with its baseline origin at `(x, y)`.
    ///
    /// Panics on a character the fixture font cannot encode — better a loud test-only
    /// failure than a fixture that silently produces different bytes than intended.
    pub fn text(mut self, x: f64, y: f64, size: f64, text: &str) -> Self {
        assert!(
            text.chars().all(|ch| ch.is_ascii() && !ch.is_control()),
            "fixture text must be printable ASCII, got {text:?}"
        );
        self.texts.push((x, y, size, text.to_owned()));
        self
    }

    /// A row of cells at fixed x positions — the shape of a catalogue table.
    pub fn row(mut self, y: f64, cells: &[(f64, &str)]) -> Self {
        for (x, text) in cells {
            self = self.text(*x, y, 11.0, text);
        }
        self
    }

    /// Put an image on the page, which is what makes it look like a scan.
    pub fn with_image(mut self) -> Self {
        self.image = true;
        self
    }
}

/// A whole document.
#[derive(Debug, Clone, Default)]
pub struct PdfBuilder {
    pages: Vec<PageBuilder>,
}

impl PdfBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn page(mut self, page: PageBuilder) -> Self {
        self.pages.push(page);
        self
    }

    /// Serialise a complete, cross-referenced PDF.
    pub fn build(&self) -> Vec<u8> {
        assert!(
            !self.pages.is_empty(),
            "a fixture PDF needs at least one page"
        );

        // Fixed object numbers for the shared objects, then two per page.
        const CATALOG: usize = 1;
        const PAGES: usize = 2;
        const FONT: usize = 3;
        const IMAGE: usize = 4;
        let first_page_object = 5;

        let mut objects: Vec<String> = Vec::new();
        let page_ids: Vec<usize> = (0..self.pages.len())
            .map(|index| first_page_object + index * 2)
            .collect();

        let kids = page_ids
            .iter()
            .map(|id| format!("{id} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");

        objects.push(format!("<</Type/Catalog/Pages {PAGES} 0 R>>"));
        objects.push(format!(
            "<</Type/Pages/Kids[{kids}]/Count {}>>",
            self.pages.len()
        ));
        objects.push(
            "<</Type/Font/Subtype/Type1/BaseFont/Helvetica/Encoding/WinAnsiEncoding>>".to_owned(),
        );
        objects.push(image_object());
        debug_assert_eq!(objects.len(), IMAGE);

        for (index, page) in self.pages.iter().enumerate() {
            let content_id = page_ids[index] + 1;
            let resources = if page.image {
                format!("<</Font<</F1 {FONT} 0 R>>/XObject<</Im1 {IMAGE} 0 R>>>>")
            } else {
                format!("<</Font<</F1 {FONT} 0 R>>>>")
            };
            objects.push(format!(
                "<</Type/Page/Parent {PAGES} 0 R/MediaBox[0 0 {:.0} {:.0}]/Resources {resources}/Contents {content_id} 0 R>>",
                page.width, page.height
            ));
            objects.push(content_object(page));
        }
        let _ = CATALOG;

        assemble(&objects)
    }
}

/// A 2×2 RGB image whose samples are spaces.
///
/// The bytes matter: a reader that walks into the image stream must find something
/// harmless there. Filling it with `0x20` keeps that true without a compression filter.
fn image_object() -> String {
    let data = " ".repeat(12);
    format!(
        "<</Type/XObject/Subtype/Image/Width 2/Height 2/ColorSpace/DeviceRGB/BitsPerComponent 8/Length {}>>\nstream\n{data}\nendstream",
        data.len()
    )
}

fn content_object(page: &PageBuilder) -> String {
    let mut content = String::new();

    if page.image {
        content.push_str(&format!(
            "q {:.0} 0 0 {:.0} 0 0 cm /Im1 Do Q\n",
            page.width, page.height
        ));
    }

    for (x, y, size, text) in &page.texts {
        content.push_str(&format!(
            "BT /F1 {size} Tf 1 0 0 1 {x} {y} Tm ({}) Tj ET\n",
            escape(text)
        ));
    }

    format!("<</Length {}>>\nstream\n{content}endstream", content.len())
}

/// `(`, `)` and `\` are the only characters a PDF literal string must escape.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        if matches!(ch, '(' | ')' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out
}

/// Write the body, the cross-reference table and the trailer.
fn assemble(objects: &[String]) -> Vec<u8> {
    let mut out: Vec<u8> = Vec::new();
    out.extend_from_slice(b"%PDF-1.4\n");
    // A binary comment marks the file as containing binary data, as real producers do.
    out.extend_from_slice(b"%\xE2\xE3\xCF\xD3\n");

    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", index + 1).as_bytes());
    }

    let xref_offset = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<</Size {}/Root 1 0 R>>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );

    out
}

// --- ready-made documents ----------------------------------------------------------

/// A one-page document with a heading and a paragraph: the "PDF with a text layer" case.
pub fn text_pdf() -> Vec<u8> {
    PdfBuilder::new()
        .page(
            PageBuilder::new()
                .text(50.0, 780.0, 24.0, "BASIS mounting systems")
                .text(
                    50.0,
                    700.0,
                    11.0,
                    "The catalogue describes profiles, consoles",
                )
                .text(50.0, 686.0, 11.0, "and connectors for engineering systems.")
                .text(
                    50.0,
                    672.0,
                    11.0,
                    "Every item is supplied with mounting hardware",
                )
                .text(50.0, 658.0, 11.0, "and a certificate of conformity.")
                .text(50.0, 40.0, 7.0, "1/1 basisparts.ru"),
        )
        .build()
}

/// A document whose pages carry an image and no text at all: the scanned presentation.
pub fn scanned_pdf(pages: usize) -> Vec<u8> {
    let mut builder = PdfBuilder::new();
    for _ in 0..pages {
        builder = builder.page(PageBuilder::landscape().with_image());
    }
    builder.build()
}

/// Mixed document: a readable page, a scanned page, and a page with a table.
pub fn mixed_pdf() -> Vec<u8> {
    PdfBuilder::new()
        .page(
            PageBuilder::new()
                .text(50.0, 780.0, 20.0, "BASIS mounting systems")
                .text(50.0, 700.0, 11.0, "Profiles, consoles and connectors")
                .text(50.0, 686.0, 11.0, "for engineering systems of any scale")
                .text(50.0, 672.0, 11.0, "with certified mounting hardware."),
        )
        .page(PageBuilder::new().with_image())
        .page(table_page())
        .build()
}

/// A single page holding a four-row specification table.
pub fn table_pdf() -> Vec<u8> {
    PdfBuilder::new().page(table_page()).build()
}

fn table_page() -> PageBuilder {
    PageBuilder::new()
        .text(50.0, 780.0, 18.0, "Profile load table")
        .row(
            700.0,
            &[
                (50.0, "Profile"),
                (220.0, "Length, mm"),
                (400.0, "Load (kN)"),
            ],
        )
        .row(680.0, &[(50.0, "BP21"), (220.0, "1200"), (400.0, "3.5")])
        .row(660.0, &[(50.0, "BP21D"), (220.0, "1500"), (400.0, "4.2")])
        .row(640.0, &[(50.0, "BP30"), (220.0, "1800"), (400.0, "6.0")])
        // The load of this profile is not printed in the source.
        .row(620.0, &[(50.0, "BP40"), (220.0, "2000")])
}

/// A file that is not a PDF at all.
pub fn not_a_pdf() -> Vec<u8> {
    b"%PDF-1.4 this header lies; the rest is not a document".to_vec()
}
