//! REQ-NF-007 defines layout parity as the per-page rate
//! `|Ferritex breaks △ pdfLaTeX breaks| / max(1, |pdfLaTeX breaks|)`,
//! averaged across the pages of a document.
//!
//! This module implements a bounded approximation of that metric by extracting
//! line y-coordinates from uncompressed PDF content streams. It inspects
//! `BT`/`ET` text blocks, follows text positioning operators such as `Td`,
//! `TD`, `Tm`, `T*`, `'`, and `"`, and quantizes observed y-values to 1pt.
//!
//! This is faithful enough for the layout-core harness because Ferritex and the
//! pdfLaTeX reference PDFs share the same page dimensions and font sizes, so
//! matching line-break decisions show up as matching line baselines.
//!
//! Limitations:
//! - Only uncompressed content streams are supported. If a content stream uses
//!   `/Filter /FlateDecode`, extraction returns an error instead of silently
//!   producing incorrect measurements.
//! - The approximation only measures line y-position parity; it does not verify
//!   the exact character-level break location within a line.

use std::collections::BTreeSet;

#[derive(Debug, Clone)]
pub struct ParityScore {
    pub page_count_match: bool,
    pub ferritex_pages: usize,
    pub reference_pages: usize,
    pub per_page_diff_rates: Vec<f64>,
    pub document_diff_rate: f64,
    pub pass: bool,
}

#[derive(Debug, Clone)]
pub struct ParityResult {
    pub document_name: String,
    pub score: Option<ParityScore>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PdfIndirectRef {
    object_number: u32,
    generation: u16,
}

pub fn extract_pdf_page_count(pdf_bytes: &[u8]) -> Result<usize, String> {
    if !is_pdf_signature(pdf_bytes) {
        return Err("input is not a PDF".to_string());
    }

    let mut offset = 0usize;
    let mut counted_pages = 0usize;
    let mut pages_dict_count = None;

    while let Some((_, object_body, next_offset)) = next_pdf_object(pdf_bytes, offset) {
        if object_contains_pages_type(object_body) {
            if let Some(count) = extract_pages_count(object_body) {
                pages_dict_count = Some(count);
                break;
            }
        } else if object_contains_page_type(object_body) {
            counted_pages += 1;
        }
        offset = next_offset;
    }

    pages_dict_count
        .or((counted_pages > 0).then_some(counted_pages))
        .ok_or_else(|| "failed to extract PDF page count".to_string())
}

pub fn extract_line_y_positions(pdf_bytes: &[u8]) -> Result<Vec<Vec<i64>>, String> {
    if !is_pdf_signature(pdf_bytes) {
        return Err("input is not a PDF".to_string());
    }

    let page_objects = collect_page_objects(pdf_bytes);
    if page_objects.is_empty() {
        return Err("failed to find any PDF page objects".to_string());
    }

    let mut pages = Vec::with_capacity(page_objects.len());
    for page_object in page_objects {
        let mut y_positions = BTreeSet::new();
        for content_ref in extract_contents_references(page_object) {
            let stream = extract_uncompressed_stream(pdf_bytes, content_ref)?;
            y_positions.extend(extract_line_positions_from_stream(&stream));
        }
        pages.push(y_positions.into_iter().collect());
    }

    Ok(pages)
}

pub fn compute_parity_score(
    ferritex_pdf: &[u8],
    reference_pdf: &[u8],
) -> Result<ParityScore, String> {
    let ferritex_pages = extract_pdf_page_count(ferritex_pdf)?;
    let reference_pages = extract_pdf_page_count(reference_pdf)?;
    let ferritex_y_positions = extract_line_y_positions(ferritex_pdf)?;
    let reference_y_positions = extract_line_y_positions(reference_pdf)?;

    let common_pages = ferritex_y_positions.len().min(reference_y_positions.len());
    let mut per_page_diff_rates = Vec::with_capacity(common_pages);

    for page_index in 0..common_pages {
        let ferritex_lines = ferritex_y_positions[page_index]
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let reference_lines = reference_y_positions[page_index]
            .iter()
            .copied()
            .collect::<BTreeSet<_>>();
        let symmetric_diff = ferritex_lines
            .symmetric_difference(&reference_lines)
            .count();
        let denominator = reference_lines.len().max(1) as f64;
        per_page_diff_rates.push(symmetric_diff as f64 / denominator);
    }

    let document_diff_rate = if per_page_diff_rates.is_empty() {
        0.0
    } else {
        per_page_diff_rates.iter().sum::<f64>() / per_page_diff_rates.len() as f64
    };
    let page_count_match = ferritex_pages == reference_pages;
    let pass = page_count_match && document_diff_rate <= 0.05;

    Ok(ParityScore {
        page_count_match,
        ferritex_pages,
        reference_pages,
        per_page_diff_rates,
        document_diff_rate,
        pass,
    })
}

pub fn format_parity_summary(results: &[ParityResult]) -> String {
    let document_width = results
        .iter()
        .map(|result| result.document_name.len())
        .max()
        .unwrap_or(8)
        .max("Document".len());

    let mut lines = vec![
        "REQ-NF-007 Parity Summary (layout-core)".to_string(),
        "========================================".to_string(),
        format!(
            "{:<document_width$} {:>7} {:>7} {}",
            "Document",
            "Score",
            "Pages",
            "Result",
            document_width = document_width
        ),
        "-".repeat(document_width + 24),
    ];

    let mut measured = 0usize;
    let mut pass = 0usize;
    let mut fail = 0usize;
    let mut error = 0usize;

    for result in results {
        match (&result.score, &result.error) {
            (Some(score), _) => {
                measured += 1;
                if score.pass {
                    pass += 1;
                } else {
                    fail += 1;
                }
                lines.push(format!(
                    "{:<document_width$} {:>7.3} {:>7} {}",
                    result.document_name,
                    score.document_diff_rate,
                    format!("{}/{}", score.ferritex_pages, score.reference_pages),
                    if score.pass { "PASS" } else { "FAIL" },
                    document_width = document_width
                ));
            }
            (None, Some(message)) => {
                error += 1;
                lines.push(format!(
                    "{:<document_width$} {:>7} {:>7} ERROR: {}",
                    result.document_name,
                    "-",
                    "-",
                    single_line(message),
                    document_width = document_width
                ));
            }
            (None, None) => {
                error += 1;
                lines.push(format!(
                    "{:<document_width$} {:>7} {:>7} ERROR: no parity score recorded",
                    result.document_name,
                    "-",
                    "-",
                    document_width = document_width
                ));
            }
        }
    }

    lines.push("-".repeat(document_width + 24));
    lines.push(format!(
        "Total: {} measured, {} pass, {} fail, {} error",
        measured, pass, fail, error
    ));
    lines.join("\n")
}

fn is_pdf_signature(data: &[u8]) -> bool {
    data.starts_with(b"%PDF-")
}

fn collect_page_objects(data: &[u8]) -> Vec<&[u8]> {
    let mut pages = Vec::new();
    let mut offset = 0usize;
    while let Some((_, object_body, next_offset)) = next_pdf_object(data, offset) {
        if object_contains_page_type(object_body) {
            pages.push(object_body);
        }
        offset = next_offset;
    }
    pages
}

fn next_pdf_object(data: &[u8], search_from: usize) -> Option<(PdfIndirectRef, &[u8], usize)> {
    let mut index = search_from;
    while index < data.len() {
        if !data[index].is_ascii_digit() || (index > 0 && !data[index - 1].is_ascii_whitespace()) {
            index += 1;
            continue;
        }

        let object_number_end = index
            + data[index..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
        if object_number_end == index {
            index += 1;
            continue;
        }

        let generation_start = skip_pdf_whitespace(data, object_number_end);
        let generation_end = generation_start
            + data[generation_start..]
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .count();
        if generation_end == generation_start {
            index += 1;
            continue;
        }

        let obj_start = skip_pdf_whitespace(data, generation_end);
        if data.get(obj_start..obj_start + 3) != Some(b"obj")
            || matches!(data.get(obj_start + 3), Some(byte) if !byte.is_ascii_whitespace())
        {
            index += 1;
            continue;
        }

        let object_number = std::str::from_utf8(&data[index..object_number_end])
            .ok()?
            .parse::<u32>()
            .ok()?;
        let generation = std::str::from_utf8(&data[generation_start..generation_end])
            .ok()?
            .parse::<u16>()
            .ok()?;

        let body_start = obj_start + 3;
        let endobj_offset = find_bytes(&data[body_start..], b"endobj")?;
        let body_end = body_start + endobj_offset;
        return Some((
            PdfIndirectRef {
                object_number,
                generation,
            },
            &data[body_start..body_end],
            body_end + b"endobj".len(),
        ));
    }
    None
}

fn find_object_by_ref(data: &[u8], reference: PdfIndirectRef) -> Option<&[u8]> {
    let mut offset = 0usize;
    while let Some((object_ref, object_body, next_offset)) = next_pdf_object(data, offset) {
        if object_ref == reference {
            return Some(object_body);
        }
        offset = next_offset;
    }
    None
}

fn object_contains_page_type(object_body: &[u8]) -> bool {
    contains_name(object_body, b"/Type /Page")
}

fn object_contains_pages_type(object_body: &[u8]) -> bool {
    contains_name(object_body, b"/Type /Pages")
}

fn contains_name(object_body: &[u8], needle: &[u8]) -> bool {
    let mut offset = 0usize;
    while let Some(found) = find_bytes(&object_body[offset..], needle) {
        let boundary = offset + found + needle.len();
        if !matches!(
            object_body.get(boundary),
            Some(byte) if byte.is_ascii_alphanumeric() || matches!(byte, b'#' | b'-' | b'_')
        ) {
            return true;
        }
        offset = boundary;
    }
    false
}

fn extract_pages_count(object_body: &[u8]) -> Option<usize> {
    let count_start = find_bytes(object_body, b"/Count")?;
    let value_start = skip_pdf_whitespace(object_body, count_start + b"/Count".len());
    let value_end = value_start
        + object_body[value_start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
    if value_end == value_start {
        return None;
    }

    std::str::from_utf8(&object_body[value_start..value_end])
        .ok()?
        .parse()
        .ok()
}

fn extract_contents_references(object_body: &[u8]) -> Vec<PdfIndirectRef> {
    let Some(contents_start) = find_bytes(object_body, b"/Contents") else {
        return Vec::new();
    };
    let contents_start = skip_pdf_whitespace(object_body, contents_start + b"/Contents".len());

    if object_body.get(contents_start) == Some(&b'[') {
        let Some(array_end) = object_body[contents_start..]
            .iter()
            .position(|byte| *byte == b']')
            .map(|offset| contents_start + offset)
        else {
            return Vec::new();
        };
        return parse_indirect_refs(&object_body[contents_start + 1..array_end]);
    }

    parse_indirect_ref(&object_body[contents_start..])
        .map(|reference| vec![reference])
        .unwrap_or_default()
}

fn parse_indirect_refs(data: &[u8]) -> Vec<PdfIndirectRef> {
    let mut references = Vec::new();
    let mut index = 0usize;

    while index < data.len() {
        if let Some((reference, next_index)) = parse_indirect_ref_with_len(&data[index..]) {
            references.push(reference);
            index += next_index;
        } else {
            index += 1;
        }
    }

    references
}

fn parse_indirect_ref(data: &[u8]) -> Option<PdfIndirectRef> {
    parse_indirect_ref_with_len(data).map(|(reference, _)| reference)
}

fn parse_indirect_ref_with_len(data: &[u8]) -> Option<(PdfIndirectRef, usize)> {
    let object_number_start = skip_pdf_whitespace(data, 0);
    let object_number_end = object_number_start
        + data[object_number_start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
    if object_number_end == object_number_start {
        return None;
    }

    let generation_start = skip_pdf_whitespace(data, object_number_end);
    let generation_end = generation_start
        + data[generation_start..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
    if generation_end == generation_start {
        return None;
    }

    let reference_marker = skip_pdf_whitespace(data, generation_end);
    if data.get(reference_marker) != Some(&b'R') {
        return None;
    }

    Some((
        PdfIndirectRef {
            object_number: std::str::from_utf8(&data[object_number_start..object_number_end])
                .ok()?
                .parse()
                .ok()?,
            generation: std::str::from_utf8(&data[generation_start..generation_end])
                .ok()?
                .parse()
                .ok()?,
        },
        reference_marker + 1,
    ))
}

fn extract_uncompressed_stream(data: &[u8], reference: PdfIndirectRef) -> Result<Vec<u8>, String> {
    let object_body = find_object_by_ref(data, reference).ok_or_else(|| {
        format!(
            "failed to find content stream object {} {} R",
            reference.object_number, reference.generation
        )
    })?;
    let stream_start = find_bytes(object_body, b"stream")
        .ok_or_else(|| "PDF content object did not contain a stream".to_string())?;
    let header = &object_body[..stream_start];

    if find_bytes(header, b"/FlateDecode").is_some() {
        return Err("compressed PDF content streams are unsupported".to_string());
    }
    if find_bytes(header, b"/Filter").is_some() {
        return Err("filtered PDF content streams are unsupported".to_string());
    }

    let mut content_start = stream_start + b"stream".len();
    if object_body.get(content_start..content_start + 2) == Some(b"\r\n") {
        content_start += 2;
    } else if matches!(object_body.get(content_start), Some(b'\r' | b'\n')) {
        content_start += 1;
    } else {
        return Err("invalid PDF stream delimiter".to_string());
    }

    let endstream_offset = find_bytes(&object_body[content_start..], b"endstream")
        .ok_or_else(|| "PDF stream was missing endstream".to_string())?;
    let mut content_end = content_start + endstream_offset;
    if content_end >= content_start + 2
        && object_body.get(content_end - 2..content_end) == Some(b"\r\n")
    {
        content_end -= 2;
    } else if content_end > content_start
        && matches!(object_body.get(content_end - 1), Some(b'\r' | b'\n'))
    {
        content_end -= 1;
    }

    Ok(object_body[content_start..content_end].to_vec())
}

fn extract_line_positions_from_stream(stream: &[u8]) -> Vec<i64> {
    let mut positions = BTreeSet::new();
    let mut tokenizer = ContentTokenizer::new(stream);
    let mut in_text = false;
    let mut operands = Vec::new();
    let mut current_y = None;
    let mut leading = 0.0f64;

    while let Some(token) = tokenizer.next() {
        match token {
            ContentToken::Operator("BT") => {
                in_text = true;
                operands.clear();
                current_y = None;
                leading = 0.0;
            }
            ContentToken::Operator("ET") => {
                in_text = false;
                operands.clear();
                current_y = None;
                leading = 0.0;
            }
            ContentToken::Number(value) if in_text => operands.push(value),
            ContentToken::Operator(operator) if in_text => {
                match operator {
                    "Td" => {
                        if let Some(dy) = operands.iter().rev().nth(0).copied() {
                            current_y = Some(current_y.unwrap_or(0.0) + dy);
                        }
                    }
                    "TD" => {
                        if let Some(dy) = operands.iter().rev().nth(0).copied() {
                            current_y = Some(current_y.unwrap_or(0.0) + dy);
                            leading = -dy;
                        }
                    }
                    "Tm" => {
                        if let Some(y) = operands.iter().rev().nth(0).copied() {
                            current_y = Some(y);
                        }
                    }
                    "TL" => {
                        if let Some(value) = operands.last().copied() {
                            leading = value;
                        }
                    }
                    "T*" => {
                        current_y = Some(current_y.unwrap_or(0.0) - leading);
                    }
                    "Tj" | "TJ" => {
                        if let Some(y) = current_y {
                            positions.insert(y.round() as i64);
                        }
                    }
                    "'" | "\"" => {
                        current_y = Some(current_y.unwrap_or(0.0) - leading);
                        if let Some(y) = current_y {
                            positions.insert(y.round() as i64);
                        }
                    }
                    _ => {}
                }
                operands.clear();
            }
            _ => {}
        }
    }

    positions.into_iter().collect()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ContentToken<'a> {
    Number(f64),
    Operator(&'a str),
}

struct ContentTokenizer<'a> {
    data: &'a [u8],
    index: usize,
}

impl<'a> ContentTokenizer<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, index: 0 }
    }

    fn next(&mut self) -> Option<ContentToken<'a>> {
        while self.index < self.data.len() {
            self.skip_whitespace_and_comments();
            if self.index >= self.data.len() {
                return None;
            }

            match self.data[self.index] {
                b'(' => {
                    self.skip_literal_string();
                    continue;
                }
                b'[' => {
                    self.skip_bracketed(b'[', b']');
                    continue;
                }
                b'<' if self.data.get(self.index + 1) == Some(&b'<') => {
                    self.skip_bracketed_pair(b"<<", b">>");
                    continue;
                }
                b'<' => {
                    self.skip_hex_string();
                    continue;
                }
                b'/' => {
                    self.skip_name();
                    continue;
                }
                _ => {}
            }

            let start = self.index;
            while self.index < self.data.len()
                && !self.data[self.index].is_ascii_whitespace()
                && !matches!(
                    self.data[self.index],
                    b'(' | b')' | b'[' | b']' | b'<' | b'>' | b'/' | b'%'
                )
            {
                self.index += 1;
            }

            if start == self.index {
                self.index += 1;
                continue;
            }

            let token = std::str::from_utf8(&self.data[start..self.index]).ok()?;
            if let Ok(number) = token.parse::<f64>() {
                return Some(ContentToken::Number(number));
            }
            return Some(ContentToken::Operator(token));
        }

        None
    }

    fn skip_whitespace_and_comments(&mut self) {
        loop {
            while self.index < self.data.len() && self.data[self.index].is_ascii_whitespace() {
                self.index += 1;
            }
            if self.data.get(self.index) == Some(&b'%') {
                while self.index < self.data.len()
                    && !matches!(self.data[self.index], b'\r' | b'\n')
                {
                    self.index += 1;
                }
                continue;
            }
            break;
        }
    }

    fn skip_literal_string(&mut self) {
        let mut depth = 0usize;
        let mut escaped = false;

        while self.index < self.data.len() {
            let byte = self.data[self.index];
            self.index += 1;

            if escaped {
                escaped = false;
                continue;
            }

            match byte {
                b'\\' => escaped = true,
                b'(' => depth += 1,
                b')' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
    }

    fn skip_hex_string(&mut self) {
        self.index += 1;
        while self.index < self.data.len() && self.data[self.index] != b'>' {
            self.index += 1;
        }
        if self.index < self.data.len() {
            self.index += 1;
        }
    }

    fn skip_name(&mut self) {
        self.index += 1;
        while self.index < self.data.len()
            && !self.data[self.index].is_ascii_whitespace()
            && !matches!(
                self.data[self.index],
                b'(' | b')' | b'[' | b']' | b'<' | b'>' | b'/' | b'%'
            )
        {
            self.index += 1;
        }
    }

    fn skip_bracketed(&mut self, open: u8, close: u8) {
        let mut depth = 0usize;

        while self.index < self.data.len() {
            let byte = self.data[self.index];
            self.index += 1;
            if byte == open {
                depth += 1;
            } else if byte == close {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    break;
                }
            }
        }
    }

    fn skip_bracketed_pair(&mut self, open: &[u8], close: &[u8]) {
        let mut depth = 0usize;

        while self.index + 1 < self.data.len() {
            if self.data.get(self.index..self.index + open.len()) == Some(open) {
                depth += 1;
                self.index += open.len();
            } else if self.data.get(self.index..self.index + close.len()) == Some(close) {
                depth = depth.saturating_sub(1);
                self.index += close.len();
                if depth == 0 {
                    break;
                }
            } else {
                self.index += 1;
            }
        }
    }
}

fn single_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn skip_pdf_whitespace(data: &[u8], mut index: usize) -> usize {
    while matches!(data.get(index), Some(byte) if byte.is_ascii_whitespace()) {
        index += 1;
    }
    index
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::{
        compute_parity_score, extract_line_y_positions, extract_pdf_page_count,
        format_parity_summary, ParityResult, ParityScore,
    };

    const MINIMAL_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /ProcSet [/PDF] >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 18 >>\nstream\n0 0 m\n200 100 l\nS\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const LINE_POSITIONS_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 102 >>\nstream\nBT\n/F1 12 Tf\n72 720 Td\n(Hello) Tj\n0 -18 Td\n(World) Tj\n1 0 0 1 72 650 Tm\n(Again) Tj\nET\nendstream\nendobj\n5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const TWO_PAGE_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>\nendobj\n4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>\nendobj\n5 0 obj\n<< /Length 37 >>\nstream\nBT\n/F1 12 Tf\n72 720 Td\n(Page1) Tj\nET\nendstream\nendobj\n6 0 obj\n<< /Length 37 >>\nstream\nBT\n/F1 12 Tf\n72 700 Td\n(Page2) Tj\nET\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";

    #[test]
    fn extract_pdf_page_count_reads_minimal_pdf() {
        assert_eq!(extract_pdf_page_count(MINIMAL_PDF).unwrap(), 1);
    }

    #[test]
    fn extract_line_y_positions_reads_known_td_values() {
        let positions = extract_line_y_positions(LINE_POSITIONS_PDF).unwrap();

        assert_eq!(positions, vec![vec![650, 702, 720]]);
    }

    #[test]
    fn compute_parity_score_is_zero_for_identical_pdfs() {
        let score = compute_parity_score(LINE_POSITIONS_PDF, LINE_POSITIONS_PDF).unwrap();

        assert!(score.page_count_match);
        assert_eq!(score.ferritex_pages, 1);
        assert_eq!(score.reference_pages, 1);
        assert_eq!(score.per_page_diff_rates, vec![0.0]);
        assert_eq!(score.document_diff_rate, 0.0);
        assert!(score.pass);
    }

    #[test]
    fn compute_parity_score_detects_page_count_mismatch() {
        let score = compute_parity_score(LINE_POSITIONS_PDF, TWO_PAGE_PDF).unwrap();

        assert!(!score.page_count_match);
        assert_eq!(score.ferritex_pages, 1);
        assert_eq!(score.reference_pages, 2);
        assert_eq!(score.per_page_diff_rates.len(), 1);
        assert!(!score.pass);
    }

    #[test]
    fn format_parity_summary_renders_table() {
        let output = format_parity_summary(&[
            ParityResult {
                document_name: "sectioning_article".to_string(),
                score: Some(ParityScore {
                    page_count_match: true,
                    ferritex_pages: 1,
                    reference_pages: 1,
                    per_page_diff_rates: vec![0.0],
                    document_diff_rate: 0.0,
                    pass: true,
                }),
                error: None,
            },
            ParityResult {
                document_name: "combined_features".to_string(),
                score: Some(ParityScore {
                    page_count_match: true,
                    ferritex_pages: 1,
                    reference_pages: 1,
                    per_page_diff_rates: vec![0.12],
                    document_diff_rate: 0.12,
                    pass: false,
                }),
                error: None,
            },
            ParityResult {
                document_name: "compat_primitives".to_string(),
                score: None,
                error: Some("compressed PDF content streams are unsupported".to_string()),
            },
        ]);

        assert!(output.contains("REQ-NF-007 Parity Summary (layout-core)"));
        assert!(output.contains("Document"));
        assert!(output.contains("sectioning_article"));
        assert!(output.contains("0.000"));
        assert!(output.contains("combined_features"));
        assert!(output.contains("0.120"));
        assert!(output.contains("compat_primitives"));
        assert!(output.contains("ERROR: compressed PDF content streams are unsupported"));
        assert!(output.contains("Total: 2 measured, 1 pass, 1 fail, 1 error"));
    }
}
