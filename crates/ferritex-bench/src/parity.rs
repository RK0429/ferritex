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

const REQ_NF_007_MAX_DOCUMENT_DIFF_RATE: f64 = 0.05;
const TIKZ_PARITY_COORDINATE_TOLERANCE: f64 = 1.0;
const TIKZ_PARITY_MIN_MATCH_RATIO: f64 = 0.80;

#[derive(Debug, Clone)]
pub struct ParityScore {
    pub page_count_match: bool,
    pub ferritex_pages: usize,
    pub reference_pages: usize,
    pub per_page_diff_rates: Vec<f64>,
    pub document_diff_rate: f64,
    pub pass: bool,
}

impl ParityScore {
    pub fn passes_req_nf_007(&self) -> bool {
        self.page_count_match && self.document_diff_rate <= REQ_NF_007_MAX_DOCUMENT_DIFF_RATE
    }

    pub fn failure_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();

        if self.passes_req_nf_007() {
            return reasons;
        }

        if !self.page_count_match {
            reasons.push(format!(
                "page count mismatch: ferritex={}, reference={}",
                self.ferritex_pages, self.reference_pages
            ));
        }

        if !self.document_diff_rate.is_finite() {
            reasons.push(format!(
                "document_diff_rate is not finite: {}",
                self.document_diff_rate
            ));
        } else if self.document_diff_rate > REQ_NF_007_MAX_DOCUMENT_DIFF_RATE {
            reasons.push(format!(
                "document_diff_rate {:.3} exceeds threshold {:.2}",
                self.document_diff_rate, REQ_NF_007_MAX_DOCUMENT_DIFF_RATE
            ));
        }

        reasons
    }
}

#[derive(Debug, Clone)]
pub struct ParityResult {
    pub document_name: String,
    pub score: Option<ParityScore>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum GraphicsOp {
    MoveTo(f64, f64),
    LineTo(f64, f64),
    CurveTo(f64, f64, f64, f64, f64, f64),
    CurveToV(f64, f64, f64, f64),
    CurveToY(f64, f64, f64, f64),
    Rectangle(f64, f64, f64, f64),
    ClosePath,
    Stroke,
    CloseStroke,
    Fill,
    FillEvenOdd,
    FillStroke,
    FillStrokeEvenOdd,
    EndPath,
    Clip,
    ClipEvenOdd,
    ConcatMatrix(f64, f64, f64, f64, f64, f64),
    SaveState,
    RestoreState,
}

#[derive(Debug, Clone)]
pub struct TikzParityScore {
    pub ferritex_op_count: usize,
    pub reference_op_count: usize,
    pub matched_ops: usize,
    pub mismatched_ops: usize,
    pub extra_ferritex_ops: usize,
    pub extra_reference_ops: usize,
    pub coordinate_tolerance: f64,
    pub match_ratio: f64,
    pub pass: bool,
}

#[derive(Debug, Clone)]
pub struct TikzParityResult {
    pub document_name: String,
    pub score: Option<TikzParityScore>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
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
    let mut score = ParityScore {
        page_count_match,
        ferritex_pages,
        reference_pages,
        per_page_diff_rates,
        document_diff_rate,
        pass: false,
    };
    score.pass = score.passes_req_nf_007();

    Ok(score)
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
                if score.passes_req_nf_007() {
                    pass += 1;
                } else {
                    fail += 1;
                }
                lines.push(format!(
                    "{:<document_width$} {:>7.3} {:>7} {}",
                    result.document_name,
                    score.document_diff_rate,
                    format!("{}/{}", score.ferritex_pages, score.reference_pages),
                    if score.passes_req_nf_007() {
                        "PASS"
                    } else {
                        "FAIL"
                    },
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

    let failing_documents = results
        .iter()
        .filter_map(|result| {
            result.score.as_ref().and_then(|score| {
                let reasons = score.failure_reasons();
                (!reasons.is_empty()).then_some((result.document_name.as_str(), reasons))
            })
        })
        .collect::<Vec<_>>();

    if !failing_documents.is_empty() {
        lines.push(String::new());
        lines.push("Failure details:".to_string());
        for (document_name, reasons) in failing_documents {
            lines.push(format!("- {}: {}", document_name, reasons.join("; ")));
        }
    }

    lines.join("\n")
}

pub fn extract_graphics_ops(pdf_bytes: &[u8]) -> Result<Vec<Vec<GraphicsOp>>, String> {
    if !is_pdf_signature(pdf_bytes) {
        return Err("input is not a PDF".to_string());
    }

    let page_objects = collect_page_objects(pdf_bytes);
    if page_objects.is_empty() {
        return Err("failed to find any PDF page objects".to_string());
    }

    let mut pages = Vec::with_capacity(page_objects.len());
    for page_object in page_objects {
        let mut page_stream = Vec::new();
        for content_ref in extract_contents_references(page_object) {
            let stream = extract_uncompressed_stream(pdf_bytes, content_ref)?;
            page_stream.extend_from_slice(&stream);
            page_stream.push(b'\n');
        }
        pages.push(extract_graphics_ops_from_stream(&page_stream));
    }

    Ok(pages)
}

pub fn compute_tikz_parity_score(
    ferritex_pdf: &[u8],
    reference_pdf: &[u8],
) -> Result<TikzParityScore, String> {
    let ferritex_ops = normalize_graphics_ops_for_scoring(
        &extract_graphics_ops(ferritex_pdf)?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
    );
    let reference_ops = normalize_graphics_ops_for_scoring(
        &extract_graphics_ops(reference_pdf)?
            .into_iter()
            .flatten()
            .collect::<Vec<_>>(),
    );

    let ferritex_blocks = filter_scored_blocks(split_graphics_ops_into_blocks(&ferritex_ops));
    let reference_blocks = filter_scored_blocks(split_graphics_ops_into_blocks(&reference_ops));
    let ferritex_scored_ops = ferritex_blocks
        .iter()
        .flat_map(|block| block.iter().cloned())
        .collect::<Vec<_>>();
    let reference_scored_ops = reference_blocks
        .iter()
        .flat_map(|block| block.iter().cloned())
        .collect::<Vec<_>>();
    let matched_ops = count_matching_block_ops(
        &ferritex_blocks,
        &reference_blocks,
        TIKZ_PARITY_COORDINATE_TOLERANCE,
    );
    let compared = ferritex_scored_ops.len().min(reference_scored_ops.len());
    let mismatched_ops = compared.saturating_sub(matched_ops);
    let extra_ferritex_ops = ferritex_scored_ops
        .len()
        .saturating_sub(reference_scored_ops.len());
    let extra_reference_ops = reference_scored_ops
        .len()
        .saturating_sub(ferritex_scored_ops.len());
    let denominator = ferritex_scored_ops.len().max(reference_scored_ops.len());
    let match_ratio = if denominator == 0 {
        1.0
    } else {
        matched_ops as f64 / denominator as f64
    };

    Ok(TikzParityScore {
        ferritex_op_count: ferritex_scored_ops.len(),
        reference_op_count: reference_scored_ops.len(),
        matched_ops,
        mismatched_ops,
        extra_ferritex_ops,
        extra_reference_ops,
        coordinate_tolerance: TIKZ_PARITY_COORDINATE_TOLERANCE,
        match_ratio,
        pass: match_ratio >= TIKZ_PARITY_MIN_MATCH_RATIO,
    })
}

pub fn format_tikz_parity_summary(results: &[TikzParityResult]) -> String {
    let document_width = results
        .iter()
        .map(|result| result.document_name.len())
        .max()
        .unwrap_or(8)
        .max("Document".len());

    let mut lines = vec![
        "TikZ Geometric Parity Summary".to_string(),
        "=============================".to_string(),
        format!(
            "{:<document_width$} {:>7} {:>9} {:>9} {}",
            "Document",
            "Match",
            "Matched",
            "Ops",
            "Result",
            document_width = document_width
        ),
        "-".repeat(document_width + 38),
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
                    "{:<document_width$} {:>7.3} {:>9} {:>9} {}",
                    result.document_name,
                    score.match_ratio,
                    format!(
                        "{}/{}",
                        score.matched_ops,
                        score.reference_op_count.max(score.ferritex_op_count)
                    ),
                    format!("{}/{}", score.ferritex_op_count, score.reference_op_count),
                    if score.pass { "PASS" } else { "FAIL" },
                    document_width = document_width
                ));
            }
            (None, Some(message)) => {
                error += 1;
                lines.push(format!(
                    "{:<document_width$} {:>7} {:>9} {:>9} ERROR: {}",
                    result.document_name,
                    "-",
                    "-",
                    "-",
                    single_line(message),
                    document_width = document_width
                ));
            }
            (None, None) => {
                error += 1;
                lines.push(format!(
                    "{:<document_width$} {:>7} {:>9} {:>9} ERROR: no TikZ parity score recorded",
                    result.document_name,
                    "-",
                    "-",
                    "-",
                    document_width = document_width
                ));
            }
        }
    }

    lines.push("-".repeat(document_width + 38));
    lines.push(format!(
        "Total: {} measured, {} pass, {} fail, {} error",
        measured, pass, fail, error
    ));

    let failing_documents = results
        .iter()
        .filter_map(|result| {
            result
                .score
                .as_ref()
                .filter(|score| !score.pass)
                .map(|score| (result.document_name.as_str(), score))
        })
        .collect::<Vec<_>>();
    if !failing_documents.is_empty() {
        lines.push(String::new());
        lines.push("Failure details:".to_string());
        for (document_name, score) in failing_documents {
            lines.push(format!(
                "- {}: match_ratio={:.3}, matched={}, mismatched={}, extra_ferritex={}, extra_reference={}",
                document_name,
                score.match_ratio,
                score.matched_ops,
                score.mismatched_ops,
                score.extra_ferritex_ops,
                score.extra_reference_ops
            ));
        }
    }

    lines.join("\n")
}

fn is_pdf_signature(data: &[u8]) -> bool {
    data.starts_with(b"%PDF-")
}

type AffineMatrix = [f64; 6];

fn identity_matrix() -> AffineMatrix {
    [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]
}

fn multiply_affine(lhs: AffineMatrix, rhs: AffineMatrix) -> AffineMatrix {
    [
        lhs[0] * rhs[0] + lhs[2] * rhs[1],
        lhs[1] * rhs[0] + lhs[3] * rhs[1],
        lhs[0] * rhs[2] + lhs[2] * rhs[3],
        lhs[1] * rhs[2] + lhs[3] * rhs[3],
        lhs[0] * rhs[4] + lhs[2] * rhs[5] + lhs[4],
        lhs[1] * rhs[4] + lhs[3] * rhs[5] + lhs[5],
    ]
}

fn transform_point(matrix: AffineMatrix, x: f64, y: f64) -> (f64, f64) {
    (
        matrix[0] * x + matrix[2] * y + matrix[4],
        matrix[1] * x + matrix[3] * y + matrix[5],
    )
}

fn transform_vector(matrix: AffineMatrix, x: f64, y: f64) -> (f64, f64) {
    (matrix[0] * x + matrix[2] * y, matrix[1] * x + matrix[3] * y)
}

fn collect_page_objects(data: &[u8]) -> Vec<&[u8]> {
    if let Some(root_ref) = find_pages_root(data) {
        let mut page_refs = Vec::new();
        let mut visited = BTreeSet::new();
        collect_page_refs(data, root_ref, &mut visited, &mut page_refs);
        if !page_refs.is_empty() {
            return page_refs
                .into_iter()
                .filter_map(|reference| find_object_by_ref(data, reference))
                .collect();
        }
    }

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

fn object_contains_catalog_type(object_body: &[u8]) -> bool {
    contains_name(object_body, b"/Type /Catalog")
}

fn contains_name(object_body: &[u8], needle: &[u8]) -> bool {
    let mut offset = 0usize;
    while let Some(found) = find_bytes(&object_body[offset..], needle) {
        let start = offset + found;
        let boundary = start + needle.len();
        let preceded_by_name_char = start > 0
            && matches!(
                object_body.get(start - 1),
                Some(byte) if byte.is_ascii_alphanumeric() || matches!(byte, b'#' | b'-' | b'_')
            );
        let followed_by_name_char = matches!(
            object_body.get(boundary),
            Some(byte) if byte.is_ascii_alphanumeric() || matches!(byte, b'#' | b'-' | b'_')
        );
        if !preceded_by_name_char && !followed_by_name_char {
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

fn find_pages_root(data: &[u8]) -> Option<PdfIndirectRef> {
    let mut offset = 0usize;
    while let Some((_, object_body, next_offset)) = next_pdf_object(data, offset) {
        if object_contains_catalog_type(object_body) {
            return extract_indirect_ref_for_key(object_body, b"/Pages");
        }
        offset = next_offset;
    }
    None
}

fn collect_page_refs(
    data: &[u8],
    reference: PdfIndirectRef,
    visited: &mut BTreeSet<PdfIndirectRef>,
    page_refs: &mut Vec<PdfIndirectRef>,
) {
    if !visited.insert(reference) {
        return;
    }

    let Some(object_body) = find_object_by_ref(data, reference) else {
        return;
    };
    if object_contains_page_type(object_body) {
        page_refs.push(reference);
        return;
    }
    if !object_contains_pages_type(object_body) {
        return;
    }

    for kid_ref in extract_array_refs_for_key(object_body, b"/Kids") {
        collect_page_refs(data, kid_ref, visited, page_refs);
    }
}

fn extract_indirect_ref_for_key(object_body: &[u8], key: &[u8]) -> Option<PdfIndirectRef> {
    let key_start = find_bytes(object_body, key)?;
    let value_start = skip_pdf_whitespace(object_body, key_start + key.len());
    parse_indirect_ref(&object_body[value_start..])
}

fn extract_array_refs_for_key(object_body: &[u8], key: &[u8]) -> Vec<PdfIndirectRef> {
    let Some(key_start) = find_bytes(object_body, key) else {
        return Vec::new();
    };
    let array_start = skip_pdf_whitespace(object_body, key_start + key.len());
    if object_body.get(array_start) != Some(&b'[') {
        return Vec::new();
    }
    let Some(array_end) = object_body[array_start..]
        .iter()
        .position(|byte| *byte == b']')
        .map(|offset| array_start + offset)
    else {
        return Vec::new();
    };

    parse_indirect_refs(&object_body[array_start + 1..array_end])
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

fn extract_graphics_ops_from_stream(stream: &[u8]) -> Vec<GraphicsOp> {
    let mut ops = Vec::new();
    let mut tokenizer = ContentTokenizer::new(stream);
    let mut operands = Vec::new();
    let mut ctm = identity_matrix();
    let mut matrix_stack = Vec::new();

    while let Some(token) = tokenizer.next() {
        match token {
            ContentToken::Number(value) => operands.push(value),
            ContentToken::Operator(operator) => {
                match operator {
                    "m" => {
                        if let Some([x, y]) = last_operands::<2>(&operands) {
                            let (x, y) = transform_point(ctm, x, y);
                            ops.push(GraphicsOp::MoveTo(x, y));
                        }
                    }
                    "l" => {
                        if let Some([x, y]) = last_operands::<2>(&operands) {
                            let (x, y) = transform_point(ctm, x, y);
                            ops.push(GraphicsOp::LineTo(x, y));
                        }
                    }
                    "c" => {
                        if let Some([x1, y1, x2, y2, x3, y3]) = last_operands::<6>(&operands) {
                            let (x1, y1) = transform_point(ctm, x1, y1);
                            let (x2, y2) = transform_point(ctm, x2, y2);
                            let (x3, y3) = transform_point(ctm, x3, y3);
                            ops.push(GraphicsOp::CurveTo(x1, y1, x2, y2, x3, y3));
                        }
                    }
                    "v" => {
                        if let Some([x2, y2, x3, y3]) = last_operands::<4>(&operands) {
                            let (x2, y2) = transform_point(ctm, x2, y2);
                            let (x3, y3) = transform_point(ctm, x3, y3);
                            ops.push(GraphicsOp::CurveToV(x2, y2, x3, y3));
                        }
                    }
                    "y" => {
                        if let Some([x1, y1, x3, y3]) = last_operands::<4>(&operands) {
                            let (x1, y1) = transform_point(ctm, x1, y1);
                            let (x3, y3) = transform_point(ctm, x3, y3);
                            ops.push(GraphicsOp::CurveToY(x1, y1, x3, y3));
                        }
                    }
                    "re" => {
                        if let Some([x, y, w, h]) = last_operands::<4>(&operands) {
                            let (x, y) = transform_point(ctm, x, y);
                            let (w, h) = transform_vector(ctm, w, h);
                            ops.push(GraphicsOp::Rectangle(x, y, w, h));
                        }
                    }
                    "h" => ops.push(GraphicsOp::ClosePath),
                    "S" => ops.push(GraphicsOp::Stroke),
                    "s" => ops.push(GraphicsOp::CloseStroke),
                    "f" | "F" => ops.push(GraphicsOp::Fill),
                    "f*" => ops.push(GraphicsOp::FillEvenOdd),
                    "B" => ops.push(GraphicsOp::FillStroke),
                    "B*" => ops.push(GraphicsOp::FillStrokeEvenOdd),
                    "n" => ops.push(GraphicsOp::EndPath),
                    "W" => ops.push(GraphicsOp::Clip),
                    "W*" => ops.push(GraphicsOp::ClipEvenOdd),
                    "q" => {
                        matrix_stack.push(ctm);
                        ops.push(GraphicsOp::SaveState);
                    }
                    "Q" => {
                        ctm = matrix_stack.pop().unwrap_or_else(identity_matrix);
                        ops.push(GraphicsOp::RestoreState);
                    }
                    "cm" => {
                        if let Some([a, b, c, d, e, f]) = last_operands::<6>(&operands) {
                            ops.push(GraphicsOp::ConcatMatrix(a, b, c, d, e, f));
                            ctm = multiply_affine(ctm, [a, b, c, d, e, f]);
                        }
                    }
                    _ => {}
                }
                operands.clear();
            }
        }
    }

    ops
}

fn last_operands<const N: usize>(operands: &[f64]) -> Option<[f64; N]> {
    if operands.len() < N {
        return None;
    }
    Some(std::array::from_fn(|index| {
        operands[operands.len() - N + index]
    }))
}

fn normalize_graphics_ops_for_scoring(ops: &[GraphicsOp]) -> Vec<GraphicsOp> {
    let mut normalized = Vec::new();
    let mut path_ops = Vec::new();

    for op in ops.iter().cloned() {
        if matches!(
            op,
            GraphicsOp::SaveState | GraphicsOp::RestoreState | GraphicsOp::ConcatMatrix(..)
        ) {
            continue;
        }

        if is_path_construction_op(&op) {
            path_ops.push(op);
            continue;
        }

        if is_path_terminal_op(&op) {
            normalized.extend(canonicalize_path_ops(&path_ops));
            path_ops.clear();
            if !matches!(op, GraphicsOp::EndPath) {
                normalized.push(op);
            }
            continue;
        }
    }

    normalized.extend(canonicalize_path_ops(&path_ops));
    normalize_coordinate_origin(&mut normalized);
    normalized
}

fn is_path_construction_op(op: &GraphicsOp) -> bool {
    matches!(
        op,
        GraphicsOp::MoveTo(..)
            | GraphicsOp::LineTo(..)
            | GraphicsOp::CurveTo(..)
            | GraphicsOp::CurveToV(..)
            | GraphicsOp::CurveToY(..)
            | GraphicsOp::Rectangle(..)
            | GraphicsOp::ClosePath
    )
}

fn is_path_terminal_op(op: &GraphicsOp) -> bool {
    matches!(
        op,
        GraphicsOp::Stroke
            | GraphicsOp::CloseStroke
            | GraphicsOp::Fill
            | GraphicsOp::FillEvenOdd
            | GraphicsOp::FillStroke
            | GraphicsOp::FillStrokeEvenOdd
            | GraphicsOp::EndPath
            | GraphicsOp::Clip
            | GraphicsOp::ClipEvenOdd
    )
}

#[derive(Debug, Clone, PartialEq)]
enum ComparableSegment {
    Line {
        start: (f64, f64),
        end: (f64, f64),
    },
    Curve {
        start: (f64, f64),
        control1: (f64, f64),
        control2: (f64, f64),
        end: (f64, f64),
    },
}

impl ComparableSegment {
    fn start(&self) -> (f64, f64) {
        match self {
            Self::Line { start, .. } | Self::Curve { start, .. } => *start,
        }
    }

    fn reversed(&self) -> Self {
        match self {
            Self::Line { start, end } => Self::Line {
                start: *end,
                end: *start,
            },
            Self::Curve {
                start,
                control1,
                control2,
                end,
            } => Self::Curve {
                start: *end,
                control1: *control2,
                control2: *control1,
                end: *start,
            },
        }
    }

    fn to_graphics_op(&self) -> GraphicsOp {
        match self {
            Self::Line { end, .. } => GraphicsOp::LineTo(end.0, end.1),
            Self::Curve {
                control1,
                control2,
                end,
                ..
            } => GraphicsOp::CurveTo(control1.0, control1.1, control2.0, control2.1, end.0, end.1),
        }
    }
}

fn canonicalize_path_ops(path_ops: &[GraphicsOp]) -> Vec<GraphicsOp> {
    let expanded = expand_path_ops(path_ops);
    let collapsed = collapse_consecutive_movetos(&expanded);
    if collapsed.is_empty() {
        return collapsed;
    }

    let Some(segments) = segments_from_path_ops(&collapsed) else {
        return collapsed;
    };
    if segments.is_empty() {
        return collapsed;
    }

    let mut segment_ops = segments
        .iter()
        .map(canonicalize_segment_ops)
        .collect::<Vec<_>>();
    let path_bounds = graphics_ops_bounds_min(
        &segment_ops
            .iter()
            .flat_map(|ops| ops.iter().cloned())
            .collect::<Vec<_>>(),
    )
    .unwrap_or((0.0, 0.0));
    segment_ops.sort_by_key(|ops| {
        (
            translation_invariant_ops_key(ops),
            ops_key_with_offset(ops, path_bounds.0, path_bounds.1),
        )
    });
    segment_ops.into_iter().flatten().collect()
}

fn expand_path_ops(path_ops: &[GraphicsOp]) -> Vec<GraphicsOp> {
    let mut expanded = Vec::new();

    for op in path_ops {
        match *op {
            GraphicsOp::Rectangle(x, y, w, h) => {
                expanded.push(GraphicsOp::MoveTo(x, y));
                expanded.push(GraphicsOp::LineTo(x + w, y));
                expanded.push(GraphicsOp::LineTo(x + w, y + h));
                expanded.push(GraphicsOp::LineTo(x, y + h));
                expanded.push(GraphicsOp::ClosePath);
            }
            _ => expanded.push(op.clone()),
        }
    }

    expanded
}

fn collapse_consecutive_movetos(path_ops: &[GraphicsOp]) -> Vec<GraphicsOp> {
    let mut collapsed = Vec::new();

    for op in path_ops {
        if let GraphicsOp::MoveTo(..) = op {
            while matches!(collapsed.last(), Some(GraphicsOp::MoveTo(..))) {
                collapsed.pop();
            }
        }
        collapsed.push(op.clone());
    }

    if collapsed.len() == 1 && matches!(collapsed[0], GraphicsOp::MoveTo(..)) {
        return Vec::new();
    }

    collapsed
}

fn segments_from_path_ops(path_ops: &[GraphicsOp]) -> Option<Vec<ComparableSegment>> {
    let mut segments = Vec::new();
    let mut current = None;
    let mut subpath_start = None;

    for op in path_ops {
        match *op {
            GraphicsOp::MoveTo(x, y) => {
                current = Some((x, y));
                subpath_start = Some((x, y));
            }
            GraphicsOp::LineTo(x, y) => {
                let start = current?;
                segments.push(ComparableSegment::Line { start, end: (x, y) });
                current = Some((x, y));
            }
            GraphicsOp::CurveTo(x1, y1, x2, y2, x3, y3) => {
                let start = current?;
                segments.push(ComparableSegment::Curve {
                    start,
                    control1: (x1, y1),
                    control2: (x2, y2),
                    end: (x3, y3),
                });
                current = Some((x3, y3));
            }
            GraphicsOp::CurveToV(x2, y2, x3, y3) => {
                let start = current?;
                segments.push(ComparableSegment::Curve {
                    start,
                    control1: start,
                    control2: (x2, y2),
                    end: (x3, y3),
                });
                current = Some((x3, y3));
            }
            GraphicsOp::CurveToY(x1, y1, x3, y3) => {
                let start = current?;
                segments.push(ComparableSegment::Curve {
                    start,
                    control1: (x1, y1),
                    control2: (x3, y3),
                    end: (x3, y3),
                });
                current = Some((x3, y3));
            }
            GraphicsOp::ClosePath => {
                let current_point = current?;
                let subpath_start = subpath_start?;
                if !points_within_tolerance(current_point, subpath_start, 1e-6) {
                    segments.push(ComparableSegment::Line {
                        start: current_point,
                        end: subpath_start,
                    });
                }
                current = Some(subpath_start);
            }
            GraphicsOp::Rectangle(..)
            | GraphicsOp::Stroke
            | GraphicsOp::CloseStroke
            | GraphicsOp::Fill
            | GraphicsOp::FillEvenOdd
            | GraphicsOp::FillStroke
            | GraphicsOp::FillStrokeEvenOdd
            | GraphicsOp::EndPath
            | GraphicsOp::Clip
            | GraphicsOp::ClipEvenOdd
            | GraphicsOp::ConcatMatrix(..)
            | GraphicsOp::SaveState
            | GraphicsOp::RestoreState => {}
        }
    }

    Some(segments)
}

fn canonicalize_segment_ops(segment: &ComparableSegment) -> Vec<GraphicsOp> {
    let forward = segment_to_ops(segment);
    let reversed = segment_to_ops(&segment.reversed());
    if translation_invariant_ops_key(&forward) <= translation_invariant_ops_key(&reversed) {
        forward
    } else {
        reversed
    }
}

fn segment_to_ops(segment: &ComparableSegment) -> Vec<GraphicsOp> {
    let start = segment.start();
    vec![
        GraphicsOp::MoveTo(start.0, start.1),
        segment.to_graphics_op(),
    ]
}

fn canonical_ops_key(ops: &[GraphicsOp]) -> String {
    ops.iter()
        .map(graphics_op_key)
        .collect::<Vec<_>>()
        .join("|")
}

fn translation_invariant_ops_key(ops: &[GraphicsOp]) -> String {
    let mut normalized = ops.to_vec();
    normalize_coordinate_origin(&mut normalized);
    canonical_ops_key(&normalized)
}

fn ops_key_with_offset(ops: &[GraphicsOp], dx: f64, dy: f64) -> String {
    let mut normalized = ops.to_vec();
    for op in &mut normalized {
        normalize_graphics_op_coordinates(op, dx, dy);
    }
    canonical_ops_key(&normalized)
}

fn graphics_op_key(op: &GraphicsOp) -> String {
    match *op {
        GraphicsOp::MoveTo(x, y) => format!("m:{:.3},{:.3}", x, y),
        GraphicsOp::LineTo(x, y) => format!("l:{:.3},{:.3}", x, y),
        GraphicsOp::CurveTo(x1, y1, x2, y2, x3, y3) => format!(
            "c:{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}",
            x1, y1, x2, y2, x3, y3
        ),
        GraphicsOp::CurveToV(x2, y2, x3, y3) => {
            format!("v:{:.3},{:.3},{:.3},{:.3}", x2, y2, x3, y3)
        }
        GraphicsOp::CurveToY(x1, y1, x3, y3) => {
            format!("y:{:.3},{:.3},{:.3},{:.3}", x1, y1, x3, y3)
        }
        GraphicsOp::Rectangle(x, y, w, h) => format!("re:{:.3},{:.3},{:.3},{:.3}", x, y, w, h),
        GraphicsOp::ClosePath => "h".to_string(),
        GraphicsOp::Stroke => "S".to_string(),
        GraphicsOp::CloseStroke => "s".to_string(),
        GraphicsOp::Fill => "f".to_string(),
        GraphicsOp::FillEvenOdd => "f*".to_string(),
        GraphicsOp::FillStroke => "B".to_string(),
        GraphicsOp::FillStrokeEvenOdd => "B*".to_string(),
        GraphicsOp::EndPath => "n".to_string(),
        GraphicsOp::Clip => "W".to_string(),
        GraphicsOp::ClipEvenOdd => "W*".to_string(),
        GraphicsOp::ConcatMatrix(a, b, c, d, e, f) => {
            format!("cm:{:.3},{:.3},{:.3},{:.3},{:.3},{:.3}", a, b, c, d, e, f)
        }
        GraphicsOp::SaveState => "q".to_string(),
        GraphicsOp::RestoreState => "Q".to_string(),
    }
}

fn normalize_coordinate_origin(ops: &mut [GraphicsOp]) {
    let Some((min_x, min_y)) = graphics_ops_bounds_min(ops) else {
        return;
    };

    for op in ops {
        normalize_graphics_op_coordinates(op, min_x, min_y);
    }
}

fn graphics_ops_bounds_min(ops: &[GraphicsOp]) -> Option<(f64, f64)> {
    let mut values = graphics_ops_coordinates(ops).flatten();
    let first = values.next()?;
    let mut min_x = first.0;
    let mut min_y = first.1;

    for (x, y) in values {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
    }

    Some((min_x, min_y))
}

fn graphics_ops_coordinates(ops: &[GraphicsOp]) -> impl Iterator<Item = Vec<(f64, f64)>> + '_ {
    ops.iter().map(graphics_op_coordinates)
}

fn graphics_op_coordinates(op: &GraphicsOp) -> Vec<(f64, f64)> {
    match *op {
        GraphicsOp::MoveTo(x, y) | GraphicsOp::LineTo(x, y) => vec![(x, y)],
        GraphicsOp::Rectangle(x, y, w, h) => vec![(x, y), (x + w, y + h)],
        GraphicsOp::CurveTo(x1, y1, x2, y2, x3, y3) => vec![(x1, y1), (x2, y2), (x3, y3)],
        GraphicsOp::CurveToV(x2, y2, x3, y3) => vec![(x2, y2), (x3, y3)],
        GraphicsOp::CurveToY(x1, y1, x3, y3) => vec![(x1, y1), (x3, y3)],
        GraphicsOp::ConcatMatrix(..)
        | GraphicsOp::ClosePath
        | GraphicsOp::Stroke
        | GraphicsOp::CloseStroke
        | GraphicsOp::Fill
        | GraphicsOp::FillEvenOdd
        | GraphicsOp::FillStroke
        | GraphicsOp::FillStrokeEvenOdd
        | GraphicsOp::EndPath
        | GraphicsOp::Clip
        | GraphicsOp::ClipEvenOdd
        | GraphicsOp::SaveState
        | GraphicsOp::RestoreState => Vec::new(),
    }
}

fn normalize_graphics_op_coordinates(op: &mut GraphicsOp, dx: f64, dy: f64) {
    match op {
        GraphicsOp::MoveTo(x, y) | GraphicsOp::LineTo(x, y) => {
            *x -= dx;
            *y -= dy;
        }
        GraphicsOp::CurveTo(x1, y1, x2, y2, x3, y3) => {
            *x1 -= dx;
            *y1 -= dy;
            *x2 -= dx;
            *y2 -= dy;
            *x3 -= dx;
            *y3 -= dy;
        }
        GraphicsOp::CurveToV(x2, y2, x3, y3) | GraphicsOp::CurveToY(x2, y2, x3, y3) => {
            *x2 -= dx;
            *y2 -= dy;
            *x3 -= dx;
            *y3 -= dy;
        }
        GraphicsOp::Rectangle(x, y, _, _) => {
            *x -= dx;
            *y -= dy;
        }
        GraphicsOp::ClosePath
        | GraphicsOp::Stroke
        | GraphicsOp::CloseStroke
        | GraphicsOp::Fill
        | GraphicsOp::FillEvenOdd
        | GraphicsOp::FillStroke
        | GraphicsOp::FillStrokeEvenOdd
        | GraphicsOp::EndPath
        | GraphicsOp::Clip
        | GraphicsOp::ClipEvenOdd
        | GraphicsOp::ConcatMatrix(..)
        | GraphicsOp::SaveState
        | GraphicsOp::RestoreState => {}
    }
}

fn graphics_ops_match(lhs: &GraphicsOp, rhs: &GraphicsOp, tolerance: f64) -> bool {
    match (lhs, rhs) {
        (GraphicsOp::MoveTo(x1, y1), GraphicsOp::MoveTo(x2, y2))
        | (GraphicsOp::LineTo(x1, y1), GraphicsOp::LineTo(x2, y2)) => {
            floats_match(*x1, *x2, tolerance) && floats_match(*y1, *y2, tolerance)
        }
        (
            GraphicsOp::CurveTo(a1, b1, c1, d1, e1, f1),
            GraphicsOp::CurveTo(a2, b2, c2, d2, e2, f2),
        ) => {
            floats_match(*a1, *a2, tolerance)
                && floats_match(*b1, *b2, tolerance)
                && floats_match(*c1, *c2, tolerance)
                && floats_match(*d1, *d2, tolerance)
                && floats_match(*e1, *e2, tolerance)
                && floats_match(*f1, *f2, tolerance)
        }
        (GraphicsOp::CurveToV(a1, b1, c1, d1), GraphicsOp::CurveToV(a2, b2, c2, d2))
        | (GraphicsOp::CurveToY(a1, b1, c1, d1), GraphicsOp::CurveToY(a2, b2, c2, d2))
        | (GraphicsOp::Rectangle(a1, b1, c1, d1), GraphicsOp::Rectangle(a2, b2, c2, d2)) => {
            floats_match(*a1, *a2, tolerance)
                && floats_match(*b1, *b2, tolerance)
                && floats_match(*c1, *c2, tolerance)
                && floats_match(*d1, *d2, tolerance)
        }
        (GraphicsOp::ClosePath, GraphicsOp::ClosePath)
        | (GraphicsOp::Stroke, GraphicsOp::Stroke)
        | (GraphicsOp::CloseStroke, GraphicsOp::CloseStroke)
        | (GraphicsOp::Fill, GraphicsOp::Fill)
        | (GraphicsOp::FillEvenOdd, GraphicsOp::FillEvenOdd)
        | (GraphicsOp::FillStroke, GraphicsOp::FillStroke)
        | (GraphicsOp::FillStrokeEvenOdd, GraphicsOp::FillStrokeEvenOdd)
        | (GraphicsOp::EndPath, GraphicsOp::EndPath)
        | (GraphicsOp::Clip, GraphicsOp::Clip)
        | (GraphicsOp::ClipEvenOdd, GraphicsOp::ClipEvenOdd)
        | (GraphicsOp::SaveState, GraphicsOp::SaveState)
        | (GraphicsOp::RestoreState, GraphicsOp::RestoreState) => true,
        (
            GraphicsOp::ConcatMatrix(a1, b1, c1, d1, e1, f1),
            GraphicsOp::ConcatMatrix(a2, b2, c2, d2, e2, f2),
        ) => {
            floats_match(*a1, *a2, tolerance)
                && floats_match(*b1, *b2, tolerance)
                && floats_match(*c1, *c2, tolerance)
                && floats_match(*d1, *d2, tolerance)
                && floats_match(*e1, *e2, tolerance)
                && floats_match(*f1, *f2, tolerance)
        }
        _ => false,
    }
}

fn split_graphics_ops_into_blocks(ops: &[GraphicsOp]) -> Vec<Vec<GraphicsOp>> {
    let mut blocks = Vec::new();
    let mut current = Vec::new();

    for op in ops.iter().cloned() {
        let is_terminal = is_path_terminal_op(&op);
        current.push(op);
        if is_terminal {
            blocks.push(std::mem::take(&mut current));
        }
    }

    if !current.is_empty() {
        blocks.push(current);
    }

    blocks
}

fn filter_scored_blocks(blocks: Vec<Vec<GraphicsOp>>) -> Vec<Vec<GraphicsOp>> {
    blocks
        .into_iter()
        .filter(|block| !is_ignorable_small_block(block))
        .collect()
}

fn count_matching_block_ops(
    ferritex_blocks: &[Vec<GraphicsOp>],
    reference_blocks: &[Vec<GraphicsOp>],
    tolerance: f64,
) -> usize {
    let mut used_reference = vec![false; reference_blocks.len()];
    let mut matched_ops = 0usize;

    for ferritex_block in ferritex_blocks {
        if let Some((index, _)) =
            reference_blocks
                .iter()
                .enumerate()
                .find(|(index, reference_block)| {
                    !used_reference[*index]
                        && graphics_op_sequences_match(ferritex_block, reference_block, tolerance)
                })
        {
            used_reference[index] = true;
            matched_ops += ferritex_block.len();
        }
    }

    matched_ops
}

fn graphics_op_sequences_match(lhs: &[GraphicsOp], rhs: &[GraphicsOp], tolerance: f64) -> bool {
    if lhs.len() != rhs.len() {
        return false;
    }
    if lhs.is_empty() {
        return true;
    }

    let prefix_matches = lhs[..lhs.len() - 1]
        .iter()
        .zip(rhs[..rhs.len() - 1].iter())
        .all(|(lhs, rhs)| graphics_ops_match(lhs, rhs, tolerance));
    prefix_matches && terminal_ops_compatible(lhs.last().unwrap(), rhs.last().unwrap())
}

fn terminal_ops_compatible(lhs: &GraphicsOp, rhs: &GraphicsOp) -> bool {
    if is_clip_operator(lhs) || is_clip_operator(rhs) {
        return matches!((lhs, rhs), (GraphicsOp::Clip, GraphicsOp::Clip))
            || matches!(
                (lhs, rhs),
                (GraphicsOp::ClipEvenOdd, GraphicsOp::ClipEvenOdd)
            );
    }

    is_paint_operator(lhs) && is_paint_operator(rhs)
}

fn is_paint_operator(op: &GraphicsOp) -> bool {
    matches!(
        op,
        GraphicsOp::Stroke
            | GraphicsOp::CloseStroke
            | GraphicsOp::Fill
            | GraphicsOp::FillEvenOdd
            | GraphicsOp::FillStroke
            | GraphicsOp::FillStrokeEvenOdd
    )
}

fn is_clip_operator(op: &GraphicsOp) -> bool {
    matches!(op, GraphicsOp::Clip | GraphicsOp::ClipEvenOdd)
}

fn is_ignorable_small_block(block: &[GraphicsOp]) -> bool {
    if block.is_empty() || block.iter().any(is_clip_operator) {
        return false;
    }

    let Some((min_x, min_y, max_x, max_y)) = block_bounds(block) else {
        return false;
    };
    let width = max_x - min_x;
    let height = max_y - min_y;
    width.max(height) <= 6.0
}

fn block_bounds(block: &[GraphicsOp]) -> Option<(f64, f64, f64, f64)> {
    let mut values = graphics_ops_coordinates(block).flatten();
    let first = values.next()?;
    let mut min_x = first.0;
    let mut min_y = first.1;
    let mut max_x = first.0;
    let mut max_y = first.1;

    for (x, y) in values {
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }

    Some((min_x, min_y, max_x, max_y))
}

fn floats_match(lhs: f64, rhs: f64, tolerance: f64) -> bool {
    (lhs - rhs).abs() <= tolerance
}

fn points_within_tolerance(lhs: (f64, f64), rhs: (f64, f64), tolerance: f64) -> bool {
    floats_match(lhs.0, rhs.0, tolerance) && floats_match(lhs.1, rhs.1, tolerance)
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
        compute_parity_score, compute_tikz_parity_score, extract_graphics_ops,
        extract_line_y_positions, extract_pdf_page_count, format_parity_summary,
        format_tikz_parity_summary, GraphicsOp, ParityResult, ParityScore, TikzParityResult,
        TikzParityScore,
    };

    const MINIMAL_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /ProcSet [/PDF] >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 18 >>\nstream\n0 0 m\n200 100 l\nS\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const LINE_POSITIONS_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 102 >>\nstream\nBT\n/F1 12 Tf\n72 720 Td\n(Hello) Tj\n0 -18 Td\n(World) Tj\n1 0 0 1 72 650 Tm\n(Again) Tj\nET\nendstream\nendobj\n5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const TWO_PAGE_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>\nendobj\n4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>\nendobj\n5 0 obj\n<< /Length 37 >>\nstream\nBT\n/F1 12 Tf\n72 720 Td\n(Page1) Tj\nET\nendstream\nendobj\n6 0 obj\n<< /Length 37 >>\nstream\nBT\n/F1 12 Tf\n72 700 Td\n(Page2) Tj\nET\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";

    fn pdf_with_streams(streams: &[&str]) -> Vec<u8> {
        let mut objects = Vec::new();
        objects.push("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string());
        objects.push("2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string());
        let content_refs = (0..streams.len())
            .map(|index| format!("{} 0 R", 4 + index))
            .collect::<Vec<_>>()
            .join(" ");
        let contents = if streams.len() == 1 {
            format!("4 0 R")
        } else {
            format!("[{content_refs}]")
        };
        objects.push(format!(
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {} >>\nendobj\n",
            contents
        ));

        for (index, stream) in streams.iter().enumerate() {
            let object_number = 4 + index;
            objects.push(format!(
                "{object_number} 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
                stream.len() + 1,
                stream
            ));
        }

        format!(
            "%PDF-1.4\n{}trailer\n<< /Root 1 0 R >>\n%%EOF\n",
            objects.join("")
        )
        .into_bytes()
    }

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
        assert!(score.passes_req_nf_007());
        assert!(score.failure_reasons().is_empty());
        assert!(score.pass);
    }

    #[test]
    fn compute_parity_score_detects_page_count_mismatch() {
        let score = compute_parity_score(LINE_POSITIONS_PDF, TWO_PAGE_PDF).unwrap();

        assert!(!score.page_count_match);
        assert_eq!(score.ferritex_pages, 1);
        assert_eq!(score.reference_pages, 2);
        assert_eq!(score.per_page_diff_rates.len(), 1);
        assert!(!score.passes_req_nf_007());
        assert_eq!(
            score.failure_reasons(),
            vec![
                "page count mismatch: ferritex=1, reference=2".to_string(),
                "document_diff_rate 2.000 exceeds threshold 0.05".to_string()
            ]
        );
        assert!(!score.pass);
    }

    #[test]
    fn extract_graphics_ops_applies_ctm_to_path_coordinates() {
        let pdf = pdf_with_streams(&["q 1 0 0 1 10 20 cm 0 0 m 5 0 l S Q"]);
        let ops = extract_graphics_ops(&pdf).unwrap();

        assert_eq!(
            ops,
            vec![vec![
                GraphicsOp::SaveState,
                GraphicsOp::ConcatMatrix(1.0, 0.0, 0.0, 1.0, 10.0, 20.0),
                GraphicsOp::MoveTo(10.0, 20.0),
                GraphicsOp::LineTo(15.0, 20.0),
                GraphicsOp::Stroke,
                GraphicsOp::RestoreState,
            ]]
        );
    }

    #[test]
    fn compute_tikz_parity_score_normalizes_closed_paths() {
        let ferritex_pdf =
            pdf_with_streams(&["q 1 0 0 1 72 650 cm 0 0 m 10 0 l 10 10 l 0 10 l h S Q"]);
        let reference_pdf = pdf_with_streams(&[
            "1 0 0 1 148 610 cm q q 0 0 m 0 0 m 0 10 l 10 10 l 10 0 l h S 10 10 m n Q Q 1 0 0 1 -148 -610 cm",
        ]);

        let score = compute_tikz_parity_score(&ferritex_pdf, &reference_pdf).unwrap();

        assert_eq!(score.ferritex_op_count, score.reference_op_count);
        assert_eq!(score.mismatched_ops, 0);
        assert_eq!(score.extra_ferritex_ops, 0);
        assert_eq!(score.extra_reference_ops, 0);
        assert!(score.match_ratio >= 1.0);
        assert!(score.pass);
    }

    #[test]
    fn failure_reasons_include_diff_rate_threshold_violation() {
        let score = ParityScore {
            page_count_match: true,
            ferritex_pages: 1,
            reference_pages: 1,
            per_page_diff_rates: vec![0.12],
            document_diff_rate: 0.12,
            pass: false,
        };

        assert!(!score.passes_req_nf_007());
        assert_eq!(
            score.failure_reasons(),
            vec!["document_diff_rate 0.120 exceeds threshold 0.05".to_string()]
        );
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
        assert!(output.contains("Failure details:"));
        assert!(
            output.contains("- combined_features: document_diff_rate 0.120 exceeds threshold 0.05")
        );
    }

    #[test]
    fn format_tikz_parity_summary_renders_table() {
        let output = format_tikz_parity_summary(&[
            TikzParityResult {
                document_name: "circle".to_string(),
                score: Some(TikzParityScore {
                    ferritex_op_count: 6,
                    reference_op_count: 6,
                    matched_ops: 6,
                    mismatched_ops: 0,
                    extra_ferritex_ops: 0,
                    extra_reference_ops: 0,
                    coordinate_tolerance: 1.0,
                    match_ratio: 1.0,
                    pass: true,
                }),
                error: None,
            },
            TikzParityResult {
                document_name: "arrow_styles".to_string(),
                score: None,
                error: Some("missing reference PDF".to_string()),
            },
        ]);

        assert!(output.contains("TikZ Geometric Parity Summary"));
        assert!(output.contains("circle"));
        assert!(output.contains("1.000"));
        assert!(output.contains("arrow_styles"));
        assert!(output.contains("ERROR: missing reference PDF"));
        assert!(output.contains("Total: 1 measured, 1 pass, 0 fail, 1 error"));
    }
}
