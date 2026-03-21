use crate::kernel::api::DimensionValue;
use crate::typesetting::api::{TypesetDocument, TypesetPage};

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const LEFT_MARGIN_PT: i64 = 72;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfDocument {
    pub bytes: Vec<u8>,
    pub page_count: usize,
    pub total_lines: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontResource {
    /// A bare Type1 reference (e.g., Helvetica) - no embedding
    BuiltinType1 { base_font: String },
    /// An embedded TrueType font with raw font data
    EmbeddedTrueType {
        base_font: String,
        font_data: Vec<u8>,
        /// First char code in the encoding
        first_char: u8,
        /// Last char code in the encoding
        last_char: u8,
        /// Glyph widths for first_char..=last_char, in PDF text space (1/1000 of text size)
        widths: Vec<u16>,
        /// Font bounding box [llx, lly, urx, ury] in font units
        bbox: [i16; 4],
        /// Ascent in font units
        ascent: i16,
        /// Descent in font units (negative)
        descent: i16,
        /// Italic angle in degrees (0 for upright)
        italic_angle: i16,
        /// Dominant stem width (required by PDF spec)
        stem_v: u16,
        /// Capital letter height in font units
        cap_height: i16,
        /// Units per em
        units_per_em: u16,
        /// Optional char-code to Unicode mapping for searchable/selectable text
        to_unicode_map: Option<Vec<(u16, char)>>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfRenderer {
    fonts: Vec<FontResource>,
}

impl PdfRenderer {
    pub fn new() -> Self {
        Self {
            fonts: default_font_resources(),
        }
    }

    pub fn with_fonts(fonts: Vec<FontResource>) -> Self {
        Self {
            fonts: if fonts.is_empty() {
                default_font_resources()
            } else {
                fonts
            },
        }
    }

    pub fn render(&self, document: &TypesetDocument) -> PdfDocument {
        let page_count = document.pages.len();
        let total_lines = document.pages.iter().map(|page| page.lines.len()).sum();
        let mut pdf = Vec::<u8>::new();
        let mut offsets = Vec::<usize>::new();
        let page_object_start = 3usize;
        let content_object_start = page_object_start + page_count;
        let font_object_start = content_object_start + page_count;
        let font_objects = build_font_objects(&self.fonts, font_object_start);
        let page_font_resources = page_font_resources(&font_objects);

        pdf.extend_from_slice(b"%PDF-1.4\n");
        append_object(
            &mut pdf,
            &mut offsets,
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n",
        );
        append_object(
            &mut pdf,
            &mut offsets,
            &format!(
                "2 0 obj\n<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n",
                page_kids(page_count, page_object_start),
                page_count
            ),
        );

        for (page_index, page) in document.pages.iter().enumerate() {
            let page_object_id = page_object_start + page_index;
            let content_object_id = content_object_start + page_index;
            append_object(
                &mut pdf,
                &mut offsets,
                &format!(
                    "{page_object_id} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {content_object_id} 0 R /Resources << /Font << {} >> >> >>\nendobj\n",
                    points_to_pdf_number(page.page_box.width),
                    points_to_pdf_number(page.page_box.height),
                    page_font_resources,
                ),
            );
        }

        for (page_index, page) in document.pages.iter().enumerate() {
            let content_object_id = content_object_start + page_index;
            let stream = render_page_stream(page);
            append_object(
                &mut pdf,
                &mut offsets,
                &format!(
                    "{content_object_id} 0 obj\n<< /Length {} >>\nstream\n{}endstream\nendobj\n",
                    stream.len(),
                    stream
                ),
            );
        }

        for font_object in &font_objects {
            for object in &font_object.objects {
                append_object_bytes(&mut pdf, &mut offsets, object);
            }
        }

        let xref_offset = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
                offsets.len() + 1,
                xref_offset
            )
            .as_bytes(),
        );

        PdfDocument {
            bytes: pdf,
            page_count,
            total_lines,
        }
    }
}

impl Default for PdfRenderer {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
struct FontObjectSet {
    dictionary_object_id: usize,
    objects: Vec<Vec<u8>>,
}

fn default_font_resources() -> Vec<FontResource> {
    vec![FontResource::BuiltinType1 {
        base_font: "Helvetica".to_string(),
    }]
}

fn build_font_objects(fonts: &[FontResource], start_object_id: usize) -> Vec<FontObjectSet> {
    let mut next_object_id = start_object_id;
    let mut font_objects = Vec::with_capacity(fonts.len());

    for font in fonts {
        match font {
            FontResource::BuiltinType1 { base_font } => {
                let dictionary_object_id = next_object_id;
                next_object_id += 1;

                font_objects.push(FontObjectSet {
                    dictionary_object_id,
                    objects: vec![
                        format!(
                            "{dictionary_object_id} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /{} >>\nendobj\n",
                            base_font
                        )
                        .into_bytes(),
                    ],
                });
            }
            FontResource::EmbeddedTrueType {
                base_font,
                font_data,
                first_char,
                last_char,
                widths,
                bbox,
                ascent,
                descent,
                italic_angle,
                stem_v,
                cap_height,
                units_per_em: _,
                to_unicode_map,
            } => {
                let dictionary_object_id = next_object_id;
                let descriptor_object_id = next_object_id + 1;
                let font_file_object_id = next_object_id + 2;
                let to_unicode_object_id = to_unicode_map.as_ref().map(|_| next_object_id + 3);
                next_object_id += 3 + usize::from(to_unicode_object_id.is_some());

                let mut font_file_object = format!(
                    "{font_file_object_id} 0 obj\n<< /Length {} /Length1 {} >>\nstream\n",
                    font_data.len(),
                    font_data.len()
                )
                .into_bytes();
                font_file_object.extend_from_slice(font_data);
                font_file_object.extend_from_slice(b"\nendstream\nendobj\n");

                let to_unicode_reference = to_unicode_object_id
                    .map(|object_id| format!(" /ToUnicode {object_id} 0 R"))
                    .unwrap_or_default();
                let mut objects = vec![
                    format!(
                        "{dictionary_object_id} 0 obj\n<< /Type /Font /Subtype /TrueType /BaseFont /{} /FirstChar {} /LastChar {} /Widths [{}] /FontDescriptor {} 0 R{} >>\nendobj\n",
                        base_font,
                        first_char,
                        last_char,
                        render_widths(widths),
                        descriptor_object_id,
                        to_unicode_reference
                    )
                    .into_bytes(),
                    format!(
                        "{descriptor_object_id} 0 obj\n<< /Type /FontDescriptor /FontName /{} /Flags 32 /FontBBox [{} {} {} {}] /Ascent {} /Descent {} /ItalicAngle {} /StemV {} /CapHeight {} /FontFile2 {} 0 R >>\nendobj\n",
                        base_font,
                        bbox[0],
                        bbox[1],
                        bbox[2],
                        bbox[3],
                        ascent,
                        descent,
                        italic_angle,
                        stem_v,
                        cap_height,
                        font_file_object_id
                    )
                    .into_bytes(),
                    font_file_object,
                ];
                if let (Some(object_id), Some(map)) =
                    (to_unicode_object_id, to_unicode_map.as_ref())
                {
                    let cmap = build_to_unicode_cmap(map);
                    objects.push(
                        format!(
                            "{object_id} 0 obj\n<< /Length {} >>\nstream\n{}endstream\nendobj\n",
                            cmap.len(),
                            cmap
                        )
                        .into_bytes(),
                    );
                }

                font_objects.push(FontObjectSet {
                    dictionary_object_id,
                    objects,
                });
            }
        }
    }

    font_objects
}

fn page_font_resources(font_objects: &[FontObjectSet]) -> String {
    font_objects
        .iter()
        .enumerate()
        .map(|(index, font_object)| {
            format!("/F{} {} 0 R", index + 1, font_object.dictionary_object_id)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_widths(widths: &[u16]) -> String {
    widths
        .iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(" ")
}

fn build_to_unicode_cmap(mapping: &[(u16, char)]) -> String {
    let mut map = mapping.to_vec();
    map.sort_by_key(|(code, _)| *code);

    let entries = map
        .into_iter()
        .filter(|(code, _)| *code <= 0xff)
        .map(|(code, ch)| format!("<{code:02X}> <{}>", unicode_scalar_as_utf16_hex(ch)))
        .collect::<Vec<_>>()
        .join("\n");
    let entry_count = if entries.is_empty() {
        0
    } else {
        entries.lines().count()
    };

    format!(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n{entry_count} beginbfchar\n{entries}\nendbfchar\nendcmap\nCMapVersion 1 def\nend\nend\n"
    )
}

fn unicode_scalar_as_utf16_hex(value: char) -> String {
    let mut buffer = [0u16; 2];
    value
        .encode_utf16(&mut buffer)
        .iter()
        .map(|unit| format!("{unit:04X}"))
        .collect::<String>()
}

fn page_kids(page_count: usize, page_object_start: usize) -> String {
    (0..page_count)
        .map(|index| format!("{} 0 R", page_object_start + index))
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_page_stream(page: &TypesetPage) -> String {
    let mut stream = String::from("BT\n/F1 12 Tf\n");

    if let Some(first_line) = page.lines.first() {
        stream.push_str(&format!(
            "{} {} Td\n",
            LEFT_MARGIN_PT,
            points_to_pdf_number(first_line.y)
        ));
        stream.push_str(&format!("({}) Tj\n", escape_pdf_text(&first_line.text)));

        let mut previous_y = first_line.y;
        for line in &page.lines[1..] {
            stream.push_str(&format!(
                "0 {} Td\n",
                points_to_pdf_number(line.y - previous_y)
            ));
            stream.push_str(&format!("({}) Tj\n", escape_pdf_text(&line.text)));
            previous_y = line.y;
        }
    }

    stream.push_str("ET\n");
    stream
}

fn points_to_pdf_number(value: DimensionValue) -> i64 {
    value.0 / SCALED_POINTS_PER_POINT
}

fn append_object(buffer: &mut Vec<u8>, offsets: &mut Vec<usize>, object: &str) {
    append_object_bytes(buffer, offsets, object.as_bytes());
}

fn append_object_bytes(buffer: &mut Vec<u8>, offsets: &mut Vec<usize>, object: &[u8]) {
    offsets.push(buffer.len());
    buffer.extend_from_slice(object);
}

fn escape_pdf_text(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => result.push_str("\\\\"),
            '(' => result.push_str("\\("),
            ')' => result.push_str("\\)"),
            '\r' | '\n' | '\t' => result.push(' '),
            _ => result.push(ch),
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::{FontResource, PdfRenderer};
    use crate::kernel::api::DimensionValue;
    use crate::typesetting::api::{PageBox, TextLine, TypesetDocument, TypesetPage};

    const SCALED_POINTS_PER_POINT: i64 = 65_536;

    fn points(value: i64) -> DimensionValue {
        DimensionValue(value * SCALED_POINTS_PER_POINT)
    }

    fn page(lines: &[&str]) -> TypesetPage {
        TypesetPage {
            page_box: PageBox {
                width: points(612),
                height: points(792),
            },
            lines: lines
                .iter()
                .enumerate()
                .map(|(index, text)| TextLine {
                    text: (*text).to_string(),
                    y: points(720 - (index as i64 * 18)),
                })
                .collect(),
        }
    }

    fn single_page(lines: &[&str]) -> TypesetDocument {
        TypesetDocument {
            pages: vec![page(lines)],
        }
    }

    #[test]
    fn renders_pdf_header_for_single_page_document() {
        let pdf = PdfRenderer::default().render(&single_page(&["Hello, Ferritex!"]));

        assert!(String::from_utf8_lossy(&pdf.bytes).starts_with("%PDF-1.4"));
    }

    #[test]
    fn embeds_document_text_instead_of_placeholder_text() {
        let pdf = PdfRenderer::default().render(&single_page(&["Hello, Ferritex!"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("Hello, Ferritex!"));
        assert!(!content.contains("Ferritex placeholder PDF"));
    }

    #[test]
    fn tracks_page_count_for_multi_page_documents() {
        let document = TypesetDocument {
            pages: vec![page(&["Page 1"]), page(&["Page 2"])],
        };
        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert_eq!(pdf.page_count, 2);
        assert!(content.contains("/Count 2"));
    }

    #[test]
    fn escapes_pdf_special_characters() {
        let pdf = PdfRenderer::default().render(&single_page(&[r#"A (test) \ sample"#]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains(r#"(A \(test\) \\ sample) Tj"#));
    }

    #[test]
    fn normalizes_control_whitespace_in_pdf_text() {
        let pdf = PdfRenderer::default().render(&single_page(&["A\tB\nC\rD"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("(A B C D) Tj"));
    }

    #[test]
    fn renders_builtin_font_reference_by_default() {
        let pdf = PdfRenderer::default().render(&single_page(&["Hello, Ferritex!"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"));
        assert!(content.contains("/Resources << /Font << /F1 5 0 R >> >>"));
    }

    #[test]
    fn renders_embedded_truetype_font_objects() {
        let renderer = PdfRenderer::with_fonts(vec![embedded_font_resource()]);
        let pdf = renderer.render(&single_page(&["Hello, Ferritex!"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/Subtype /TrueType"));
        assert!(content.contains("/FontDescriptor 6 0 R"));
        assert!(content.contains("/FontFile2 7 0 R"));
        assert!(content.contains("/Length1 5"));
    }

    #[test]
    fn renders_font_descriptor_with_correct_metrics() {
        let renderer = PdfRenderer::with_fonts(vec![embedded_font_resource()]);
        let pdf = renderer.render(&single_page(&["Hello, Ferritex!"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/FontBBox [-50 -200 1200 900]"));
        assert!(content.contains("/Ascent 900"));
        assert!(content.contains("/Descent -200"));
        assert!(content.contains("/ItalicAngle 0"));
        assert!(content.contains("/StemV 80"));
        assert!(content.contains("/CapHeight 700"));
    }

    #[test]
    fn renders_multiple_font_resources() {
        let renderer = PdfRenderer::with_fonts(vec![
            FontResource::BuiltinType1 {
                base_font: "Helvetica".to_string(),
            },
            embedded_font_resource(),
        ]);
        let pdf = renderer.render(&single_page(&["Hello, Ferritex!"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/Resources << /Font << /F1 5 0 R /F2 6 0 R >> >>"));
        assert!(content.contains("5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"));
        assert!(content.contains("6 0 obj\n<< /Type /Font /Subtype /TrueType"));
    }

    #[test]
    fn emits_tounicode_cmap_for_embedded_truetype_fonts() {
        let mut font = embedded_font_resource();
        if let FontResource::EmbeddedTrueType { to_unicode_map, .. } = &mut font {
            *to_unicode_map = Some(vec![(65, 'A'), (66, 'B')]);
        }
        let renderer = PdfRenderer::with_fonts(vec![font]);
        let pdf = renderer.render(&single_page(&["AB"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/ToUnicode 8 0 R"));
        assert!(content.contains("/CMapName /Adobe-Identity-UCS"));
        assert!(content.contains("<41> <0041>"));
        assert!(content.contains("<42> <0042>"));
    }

    #[test]
    fn renders_subsetted_font_stream_length() {
        let original_font_data = vec![0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
        let subsetted_font_data = vec![0, 1, 2, 3];
        let mut font = embedded_font_resource_with_data(original_font_data);
        if let FontResource::EmbeddedTrueType { font_data, .. } = &mut font {
            *font_data = subsetted_font_data.clone();
        }
        let renderer = PdfRenderer::with_fonts(vec![font]);
        let pdf = renderer.render(&single_page(&["Hello"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(subsetted_font_data.len() < 10);
        assert!(content.contains("/Length1 4"));
    }

    fn embedded_font_resource() -> FontResource {
        embedded_font_resource_with_data(vec![1, 2, 3, 4, 5])
    }

    fn embedded_font_resource_with_data(font_data: Vec<u8>) -> FontResource {
        FontResource::EmbeddedTrueType {
            base_font: "DummySans".to_string(),
            font_data,
            first_char: 32,
            last_char: 34,
            widths: vec![250, 300, 325],
            bbox: [-50, -200, 1200, 900],
            ascent: 900,
            descent: -200,
            italic_angle: 0,
            stem_v: 80,
            cap_height: 700,
            units_per_em: 1000,
            to_unicode_map: None,
        }
    }
}
