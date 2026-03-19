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

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PdfRenderer;

impl PdfRenderer {
    pub fn render(&self, document: &TypesetDocument) -> PdfDocument {
        let page_count = document.pages.len();
        let total_lines = document.pages.iter().map(|page| page.lines.len()).sum();
        let mut pdf = Vec::<u8>::new();
        let mut offsets = Vec::<usize>::new();
        let page_object_start = 3usize;
        let content_object_start = page_object_start + page_count;
        let font_object_id = content_object_start + page_count;

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
                    "{page_object_id} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {content_object_id} 0 R /Resources << /Font << /F1 {font_object_id} 0 R >> >> >>\nendobj\n",
                    points_to_pdf_number(page.page_box.width),
                    points_to_pdf_number(page.page_box.height),
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

        append_object(
            &mut pdf,
            &mut offsets,
            &format!(
                "{font_object_id} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n"
            ),
        );

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
    offsets.push(buffer.len());
    buffer.extend_from_slice(object.as_bytes());
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
    use super::PdfRenderer;
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
        let pdf = PdfRenderer.render(&single_page(&["Hello, Ferritex!"]));

        assert!(String::from_utf8_lossy(&pdf.bytes).starts_with("%PDF-1.4"));
    }

    #[test]
    fn embeds_document_text_instead_of_placeholder_text() {
        let pdf = PdfRenderer.render(&single_page(&["Hello, Ferritex!"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("Hello, Ferritex!"));
        assert!(!content.contains("Ferritex placeholder PDF"));
    }

    #[test]
    fn tracks_page_count_for_multi_page_documents() {
        let document = TypesetDocument {
            pages: vec![page(&["Page 1"]), page(&["Page 2"])],
        };
        let pdf = PdfRenderer.render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert_eq!(pdf.page_count, 2);
        assert!(content.contains("/Count 2"));
    }

    #[test]
    fn escapes_pdf_special_characters() {
        let pdf = PdfRenderer.render(&single_page(&[r#"A (test) \ sample"#]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains(r#"(A \(test\) \\ sample) Tj"#));
    }

    #[test]
    fn normalizes_control_whitespace_in_pdf_text() {
        let pdf = PdfRenderer.render(&single_page(&["A\tB\nC\rD"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("(A B C D) Tj"));
    }
}
