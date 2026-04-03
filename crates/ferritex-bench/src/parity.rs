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

    if let Some(root_body) =
        find_pages_root(pdf_bytes).and_then(|root_ref| find_object_by_ref(pdf_bytes, root_ref))
    {
        if let Some(count) = extract_pages_count(root_body) {
            return Ok(count);
        }
    }

    extract_pdf_page_count_by_sequential_scan(pdf_bytes)
}

fn extract_pdf_page_count_by_sequential_scan(pdf_bytes: &[u8]) -> Result<usize, String> {
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

#[derive(Debug, Clone)]
pub struct NavigationManifest {
    pub annotations_per_page: Vec<usize>,
    pub named_destination_count: usize,
    pub outline_entry_count: usize,
    pub outline_max_depth: usize,
    pub metadata_title: Option<String>,
    pub metadata_author: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NavigationParityScore {
    pub annotations_match: bool,
    pub destinations_match: bool,
    pub outlines_match: bool,
    pub metadata_title_match: bool,
    pub metadata_author_match: bool,
    pub ferritex_manifest: NavigationManifest,
    pub reference_manifest: NavigationManifest,
    pub pass: bool,
}

impl NavigationParityScore {
    pub fn passes_req_nf_007(&self) -> bool {
        self.pass
    }

    pub fn failure_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();

        if self.annotations_match
            && self.destinations_match
            && self.outlines_match
            && self.metadata_title_match
            && self.metadata_author_match
        {
            return reasons;
        }

        if !self.annotations_match {
            reasons.push(format!(
                "annotation counts mismatch: ferritex={:?}, reference={:?}",
                self.ferritex_manifest.annotations_per_page,
                self.reference_manifest.annotations_per_page
            ));
        }

        if !self.destinations_match {
            reasons.push(format!(
                "named destination count mismatch: ferritex={}, reference={}",
                self.ferritex_manifest.named_destination_count,
                self.reference_manifest.named_destination_count
            ));
        }

        if !self.outlines_match {
            reasons.push(format!(
                "outline mismatch: ferritex={}/{}, reference={}/{}",
                self.ferritex_manifest.outline_entry_count,
                self.ferritex_manifest.outline_max_depth,
                self.reference_manifest.outline_entry_count,
                self.reference_manifest.outline_max_depth
            ));
        }

        if !self.metadata_title_match {
            reasons.push(format!(
                "metadata Title mismatch: ferritex={:?}, reference={:?}",
                self.ferritex_manifest.metadata_title, self.reference_manifest.metadata_title
            ));
        }

        if !self.metadata_author_match {
            reasons.push(format!(
                "metadata Author mismatch: ferritex={:?}, reference={:?}",
                self.ferritex_manifest.metadata_author, self.reference_manifest.metadata_author
            ));
        }

        reasons
    }
}

#[derive(Debug, Clone)]
pub struct NavigationParityResult {
    pub document_name: String,
    pub score: Option<NavigationParityScore>,
    pub error: Option<String>,
}

pub fn extract_navigation_manifest(pdf_bytes: &[u8]) -> Result<NavigationManifest, String> {
    if !is_pdf_signature(pdf_bytes) {
        return Err("input is not a PDF".to_string());
    }

    let page_objects = collect_page_objects(pdf_bytes);
    if page_objects.is_empty() {
        return Err("failed to find any PDF page objects".to_string());
    }

    let catalog_object =
        find_catalog_object(pdf_bytes).ok_or_else(|| "failed to find PDF catalog".to_string())?;
    let annotations_per_page = page_objects
        .iter()
        .map(|page_object| extract_array_refs_for_key(page_object, b"/Annots").len())
        .collect::<Vec<_>>();
    let named_destination_count = count_named_destination_references(pdf_bytes, catalog_object);
    let (outline_entry_count, outline_max_depth) =
        extract_outline_manifest(pdf_bytes, catalog_object);
    let info_object = find_info_object(pdf_bytes);
    let metadata_title =
        info_object.and_then(|object| extract_pdf_string_for_key(object, b"/Title"));
    let metadata_author =
        info_object.and_then(|object| extract_pdf_string_for_key(object, b"/Author"));

    Ok(NavigationManifest {
        annotations_per_page,
        named_destination_count,
        outline_entry_count,
        outline_max_depth,
        metadata_title,
        metadata_author,
    })
}

pub fn compute_navigation_parity_score(
    ferritex_pdf: &[u8],
    reference_pdf: &[u8],
) -> Result<NavigationParityScore, String> {
    let ferritex_manifest = extract_navigation_manifest(ferritex_pdf)?;
    let reference_manifest = extract_navigation_manifest(reference_pdf)?;
    let annotations_match =
        ferritex_manifest.annotations_per_page == reference_manifest.annotations_per_page;
    let destinations_match =
        ferritex_manifest.named_destination_count == reference_manifest.named_destination_count;
    let outlines_match = ferritex_manifest.outline_entry_count
        == reference_manifest.outline_entry_count
        && ferritex_manifest.outline_max_depth == reference_manifest.outline_max_depth;
    let metadata_title_match = normalize_metadata(&ferritex_manifest.metadata_title)
        == normalize_metadata(&reference_manifest.metadata_title);
    let metadata_author_match = normalize_metadata(&ferritex_manifest.metadata_author)
        == normalize_metadata(&reference_manifest.metadata_author);
    let pass = annotations_match
        && destinations_match
        && outlines_match
        && metadata_title_match
        && metadata_author_match;

    Ok(NavigationParityScore {
        annotations_match,
        destinations_match,
        outlines_match,
        metadata_title_match,
        metadata_author_match,
        ferritex_manifest,
        reference_manifest,
        pass,
    })
}

fn normalize_metadata(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|metadata| !metadata.is_empty())
}

pub fn format_navigation_parity_summary(results: &[NavigationParityResult]) -> String {
    let document_width = results
        .iter()
        .map(|result| result.document_name.len())
        .max()
        .unwrap_or(8)
        .max("Document".len());

    let mut lines = vec![
        "REQ-NF-007 Navigation Parity Summary".to_string(),
        "====================================".to_string(),
        format!(
            "{:<document_width$} {:>7} {:>7} {:>8} {:>7} {}",
            "Document",
            "Annots",
            "Dests",
            "Outlines",
            "Meta",
            "Result",
            document_width = document_width
        ),
        "-".repeat(document_width + 42),
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
                    "{:<document_width$} {:>7} {:>7} {:>8} {:>7} {}",
                    result.document_name,
                    pass_fail(score.annotations_match),
                    pass_fail(score.destinations_match),
                    pass_fail(score.outlines_match),
                    pass_fail(score.metadata_title_match && score.metadata_author_match),
                    pass_fail(score.passes_req_nf_007()),
                    document_width = document_width
                ));
            }
            (None, Some(message)) => {
                error += 1;
                lines.push(format!(
                    "{:<document_width$} {:>7} {:>7} {:>8} {:>7} ERROR: {}",
                    result.document_name,
                    "-",
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
                    "{:<document_width$} {:>7} {:>7} {:>8} {:>7} ERROR: no navigation parity score recorded",
                    result.document_name,
                    "-",
                    "-",
                    "-",
                    "-",
                    document_width = document_width
                ));
            }
        }
    }

    lines.push("-".repeat(document_width + 42));
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

#[derive(Debug, Clone)]
pub struct BibliographyManifest {
    pub entry_count: usize,
    pub citation_labels: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct BibliographyParityScore {
    pub entry_count_match: bool,
    pub labels_match: bool,
    pub ferritex_manifest: BibliographyManifest,
    pub reference_manifest: BibliographyManifest,
    pub pass: bool,
}

impl BibliographyParityScore {
    pub fn passes_req_nf_007(&self) -> bool {
        self.pass
    }

    pub fn failure_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();

        if self.entry_count_match && self.labels_match {
            return reasons;
        }

        if !self.entry_count_match {
            reasons.push(format!(
                "bibliography entry count mismatch: ferritex={}, reference={}",
                self.ferritex_manifest.entry_count, self.reference_manifest.entry_count
            ));
        }

        if !self.labels_match {
            reasons.push(format!(
                "citation labels mismatch: ferritex={:?}, reference={:?}",
                self.ferritex_manifest.citation_labels, self.reference_manifest.citation_labels
            ));
        }

        reasons
    }
}

#[derive(Debug, Clone)]
pub struct BibliographyParityResult {
    pub document_name: String,
    pub score: Option<BibliographyParityScore>,
    pub error: Option<String>,
}

pub fn extract_bibliography_manifest(pdf_bytes: &[u8]) -> Result<BibliographyManifest, String> {
    if !is_pdf_signature(pdf_bytes) {
        return Err("input is not a PDF".to_string());
    }

    let page_objects = collect_page_objects(pdf_bytes);
    if page_objects.is_empty() {
        return Err("failed to find any PDF page objects".to_string());
    }

    let mut text_lines = Vec::new();
    for page_object in page_objects {
        let mut page_stream = Vec::new();
        for content_ref in extract_contents_references(page_object) {
            let stream = extract_uncompressed_stream(pdf_bytes, content_ref)?;
            page_stream.extend_from_slice(&stream);
            page_stream.push(b'\n');
        }
        text_lines.extend(extract_text_lines_from_stream(&page_stream));
    }

    let normalized_lines = text_lines
        .into_iter()
        .map(|line| normalize_bibliography_whitespace(&line))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    let bibliography_start = normalized_lines
        .iter()
        .rposition(|line| is_bibliography_heading(line))
        .map(|index| index + 1)
        .unwrap_or(0);

    let trailing_labels = normalized_lines[bibliography_start..]
        .iter()
        .filter_map(|line| extract_citation_label_from_line(line))
        .collect::<Vec<_>>();
    let citation_labels = if trailing_labels.is_empty() && bibliography_start > 0 {
        normalized_lines
            .iter()
            .filter_map(|line| extract_citation_label_from_line(line))
            .collect::<Vec<_>>()
    } else {
        trailing_labels
    };

    Ok(BibliographyManifest {
        entry_count: citation_labels.len(),
        citation_labels,
    })
}

pub fn compute_bibliography_parity_score(
    ferritex_pdf: &[u8],
    reference_pdf: &[u8],
) -> Result<BibliographyParityScore, String> {
    let ferritex_manifest = extract_bibliography_manifest(ferritex_pdf)?;
    let reference_manifest = extract_bibliography_manifest(reference_pdf)?;
    let entry_count_match = ferritex_manifest.entry_count == reference_manifest.entry_count;
    let labels_match = ferritex_manifest.citation_labels == reference_manifest.citation_labels;
    let pass = entry_count_match && labels_match;

    Ok(BibliographyParityScore {
        entry_count_match,
        labels_match,
        ferritex_manifest,
        reference_manifest,
        pass,
    })
}

pub fn format_bibliography_parity_summary(results: &[BibliographyParityResult]) -> String {
    let document_width = results
        .iter()
        .map(|result| result.document_name.len())
        .max()
        .unwrap_or(8)
        .max("Document".len());

    let mut lines = vec![
        "REQ-NF-007 Bibliography Parity Summary".to_string(),
        "======================================".to_string(),
        format!(
            "{:<document_width$} {:>7} {:>7} {}",
            "Document",
            "Entries",
            "Labels",
            "Result",
            document_width = document_width
        ),
        "-".repeat(document_width + 28),
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
                    "{:<document_width$} {:>7} {:>7} {}",
                    result.document_name,
                    pass_fail(score.entry_count_match),
                    pass_fail(score.labels_match),
                    pass_fail(score.passes_req_nf_007()),
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
                    "{:<document_width$} {:>7} {:>7} ERROR: no bibliography parity score recorded",
                    result.document_name,
                    "-",
                    "-",
                    document_width = document_width
                ));
            }
        }
    }

    lines.push("-".repeat(document_width + 28));
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

#[derive(Debug, Clone)]
pub struct EmbeddedAssetsManifest {
    pub font_names: BTreeSet<String>,
    pub image_xobject_count: usize,
    pub form_xobject_count: usize,
    pub page_count: usize,
}

#[derive(Debug, Clone)]
pub struct EmbeddedAssetsParityScore {
    pub font_set_match: bool,
    pub image_count_match: bool,
    pub form_count_match: bool,
    pub page_count_match: bool,
    pub ferritex_manifest: EmbeddedAssetsManifest,
    pub reference_manifest: EmbeddedAssetsManifest,
    pub pass: bool,
}

impl EmbeddedAssetsParityScore {
    pub fn passes_req_nf_007(&self) -> bool {
        self.pass
    }

    pub fn failure_reasons(&self) -> Vec<String> {
        let mut reasons = Vec::new();

        if self.font_set_match
            && self.image_count_match
            && self.form_count_match
            && self.page_count_match
        {
            return reasons;
        }

        if !self.font_set_match {
            reasons.push(format!(
                "font set mismatch: ferritex={:?}, reference={:?}",
                self.ferritex_manifest.font_names, self.reference_manifest.font_names
            ));
        }

        if !self.image_count_match {
            reasons.push(format!(
                "image XObject count mismatch: ferritex={}, reference={}",
                self.ferritex_manifest.image_xobject_count,
                self.reference_manifest.image_xobject_count
            ));
        }

        if !self.form_count_match {
            reasons.push(format!(
                "form XObject count mismatch: ferritex={}, reference={}",
                self.ferritex_manifest.form_xobject_count,
                self.reference_manifest.form_xobject_count
            ));
        }

        if !self.page_count_match {
            reasons.push(format!(
                "page count mismatch: ferritex={}, reference={}",
                self.ferritex_manifest.page_count, self.reference_manifest.page_count
            ));
        }

        reasons
    }
}

#[derive(Debug, Clone)]
pub struct EmbeddedAssetsParityResult {
    pub document_name: String,
    pub score: Option<EmbeddedAssetsParityScore>,
    pub error: Option<String>,
}

pub fn extract_embedded_assets_manifest(
    pdf_bytes: &[u8],
) -> Result<EmbeddedAssetsManifest, String> {
    if !is_pdf_signature(pdf_bytes) {
        return Err("input is not a PDF".to_string());
    }

    let page_count = collect_page_objects(pdf_bytes).len();
    if page_count == 0 {
        return Err("failed to find any PDF page objects".to_string());
    }

    let mut font_names = BTreeSet::new();
    let mut image_xobject_count = 0usize;
    let mut form_xobject_count = 0usize;
    let mut offset = 0usize;

    while let Some((_, object_body, next_offset)) = next_pdf_object(pdf_bytes, offset) {
        let object_dictionary = object_dictionary_bytes(object_body);
        font_names.extend(extract_pdf_name_values_for_key(
            object_dictionary,
            b"/BaseFont",
        ));

        if contains_name(object_dictionary, b"/Subtype")
            && contains_name(object_dictionary, b"/Image")
        {
            image_xobject_count += 1;
        }
        if contains_name(object_dictionary, b"/Subtype")
            && contains_name(object_dictionary, b"/Form")
        {
            form_xobject_count += 1;
        }

        offset = next_offset;
    }

    Ok(EmbeddedAssetsManifest {
        font_names,
        image_xobject_count,
        form_xobject_count,
        page_count,
    })
}

pub fn compute_embedded_assets_parity_score(
    ferritex_pdf: &[u8],
    reference_pdf: &[u8],
) -> Result<EmbeddedAssetsParityScore, String> {
    let mut ferritex_manifest = extract_embedded_assets_manifest(ferritex_pdf)?;
    let mut reference_manifest = extract_embedded_assets_manifest(reference_pdf)?;
    ferritex_manifest.font_names = normalize_pdf_font_names(&ferritex_manifest.font_names);
    reference_manifest.font_names = normalize_pdf_font_names(&reference_manifest.font_names);
    let font_set_match = ferritex_manifest.font_names == reference_manifest.font_names;
    let image_count_match =
        ferritex_manifest.image_xobject_count == reference_manifest.image_xobject_count;
    let form_count_match =
        ferritex_manifest.form_xobject_count == reference_manifest.form_xobject_count;
    let page_count_match = ferritex_manifest.page_count == reference_manifest.page_count;
    let pass = font_set_match && image_count_match && form_count_match && page_count_match;

    Ok(EmbeddedAssetsParityScore {
        font_set_match,
        image_count_match,
        form_count_match,
        page_count_match,
        ferritex_manifest,
        reference_manifest,
        pass,
    })
}

fn normalize_pdf_font_names(font_names: &BTreeSet<String>) -> BTreeSet<String> {
    font_names
        .iter()
        .map(|font_name| match font_name.split_once('+') {
            Some((_, suffix)) => suffix.to_string(),
            None => font_name.clone(),
        })
        .collect()
}

pub fn format_embedded_assets_parity_summary(results: &[EmbeddedAssetsParityResult]) -> String {
    let document_width = results
        .iter()
        .map(|result| result.document_name.len())
        .max()
        .unwrap_or(8)
        .max("Document".len());

    let mut lines = vec![
        "REQ-NF-007 Embedded Assets Parity Summary".to_string(),
        "==========================================".to_string(),
        format!(
            "{:<document_width$} {:>7} {:>7} {:>7} {:>7} {}",
            "Document",
            "Fonts",
            "Images",
            "Forms",
            "Pages",
            "Result",
            document_width = document_width
        ),
        "-".repeat(document_width + 42),
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
                    "{:<document_width$} {:>7} {:>7} {:>7} {:>7} {}",
                    result.document_name,
                    pass_fail(score.font_set_match),
                    pass_fail(score.image_count_match),
                    pass_fail(score.form_count_match),
                    pass_fail(score.page_count_match),
                    pass_fail(score.passes_req_nf_007()),
                    document_width = document_width
                ));
            }
            (None, Some(message)) => {
                error += 1;
                lines.push(format!(
                    "{:<document_width$} {:>7} {:>7} {:>7} {:>7} ERROR: {}",
                    result.document_name,
                    "-",
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
                    "{:<document_width$} {:>7} {:>7} {:>7} {:>7} ERROR: no embedded-assets parity score recorded",
                    result.document_name,
                    "-",
                    "-",
                    "-",
                    "-",
                    document_width = document_width
                ));
            }
        }
    }

    lines.push("-".repeat(document_width + 42));
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
    find_object_in_object_streams(data, reference)
}

fn find_catalog_object(data: &[u8]) -> Option<&[u8]> {
    if let Some(root_ref) = find_root_reference(data) {
        return find_object_by_ref(data, root_ref);
    }

    let mut offset = 0usize;
    while let Some((_, object_body, next_offset)) = next_pdf_object(data, offset) {
        if object_contains_catalog_type(object_body) {
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

fn object_contains_objstm_type(object_body: &[u8]) -> bool {
    contains_name(object_dictionary_bytes(object_body), b"/Type /ObjStm")
}

fn object_contains_xref_type(object_body: &[u8]) -> bool {
    contains_name(object_dictionary_bytes(object_body), b"/Type /XRef")
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
    find_catalog_object(data).and_then(|catalog| extract_indirect_ref_for_key(catalog, b"/Pages"))
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
    let object_body = object_dictionary_bytes(object_body);
    let key_start = find_key_offset(object_body, key)?;
    let value_start = skip_pdf_whitespace(object_body, key_start + key.len());
    parse_indirect_ref(&object_body[value_start..])
}

fn extract_array_refs_for_key(object_body: &[u8], key: &[u8]) -> Vec<PdfIndirectRef> {
    let object_body = object_dictionary_bytes(object_body);
    let Some(key_start) = find_key_offset(object_body, key) else {
        return Vec::new();
    };
    let array_start = skip_pdf_whitespace(object_body, key_start + key.len());
    if object_body.get(array_start) != Some(&b'[') {
        return Vec::new();
    }
    let Some(array_end) = find_matching_array_end(object_body, array_start) else {
        return Vec::new();
    };

    parse_indirect_refs(&object_body[array_start + 1..array_end - 1])
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
    Ok(extract_uncompressed_stream_slice(data, reference)?.to_vec())
}

fn extract_uncompressed_stream_slice<'a>(
    data: &'a [u8],
    reference: PdfIndirectRef,
) -> Result<&'a [u8], String> {
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

    Ok(&object_body[content_start..content_end])
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

#[derive(Debug, Clone, PartialEq)]
enum TextOperand {
    Number(f64),
    Text(String),
}

fn extract_text_lines_from_stream(stream: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let mut tokenizer = TextContentTokenizer::new(stream);
    let mut in_text = false;
    let mut operands = Vec::new();
    let mut current_y = None;
    let mut leading = 0.0f64;
    let mut current_line_key = None;
    let mut current_line = String::new();
    let mut implicit_line_key = 0i64;

    while let Some(token) = tokenizer.next() {
        match token {
            TextContentToken::Operator("BT") => {
                flush_text_line(&mut lines, &mut current_line);
                in_text = true;
                operands.clear();
                current_y = None;
                leading = 0.0;
                current_line_key = None;
            }
            TextContentToken::Operator("ET") => {
                flush_text_line(&mut lines, &mut current_line);
                in_text = false;
                operands.clear();
                current_y = None;
                leading = 0.0;
                current_line_key = None;
            }
            TextContentToken::Number(value) if in_text => operands.push(TextOperand::Number(value)),
            TextContentToken::Text(value) if in_text => operands.push(TextOperand::Text(value)),
            TextContentToken::Operator(operator) if in_text => {
                match operator {
                    "Td" => {
                        if let Some(dy) = last_number_operand(&operands, 0) {
                            current_y = Some(current_y.unwrap_or(0.0) + dy);
                        }
                    }
                    "TD" => {
                        if let Some(dy) = last_number_operand(&operands, 0) {
                            current_y = Some(current_y.unwrap_or(0.0) + dy);
                            leading = -dy;
                        }
                    }
                    "Tm" => {
                        if let Some(y) = last_number_operand(&operands, 0) {
                            current_y = Some(y);
                        }
                    }
                    "TL" => {
                        if let Some(value) = last_number_operand(&operands, 0) {
                            leading = value;
                        }
                    }
                    "T*" => {
                        current_y = Some(current_y.unwrap_or(0.0) - leading);
                    }
                    "Tj" | "TJ" => {
                        if let Some(text) = last_text_operand(&operands) {
                            append_text_segment(
                                &mut lines,
                                &mut current_line_key,
                                &mut current_line,
                                &mut implicit_line_key,
                                current_y,
                                text,
                            );
                        }
                    }
                    "'" | "\"" => {
                        current_y = Some(current_y.unwrap_or(0.0) - leading);
                        if let Some(text) = last_text_operand(&operands) {
                            append_text_segment(
                                &mut lines,
                                &mut current_line_key,
                                &mut current_line,
                                &mut implicit_line_key,
                                current_y,
                                text,
                            );
                        }
                    }
                    _ => {}
                }
                operands.clear();
            }
            _ => {}
        }
    }

    flush_text_line(&mut lines, &mut current_line);
    lines
}

fn append_text_segment(
    lines: &mut Vec<String>,
    current_line_key: &mut Option<i64>,
    current_line: &mut String,
    implicit_line_key: &mut i64,
    current_y: Option<f64>,
    text: &str,
) {
    if text.is_empty() {
        return;
    }

    let line_key = current_y
        .map(|value| value.round() as i64)
        .or(*current_line_key)
        .unwrap_or_else(|| {
            *implicit_line_key += 1;
            *implicit_line_key
        });
    if *current_line_key != Some(line_key) {
        flush_text_line(lines, current_line);
        *current_line_key = Some(line_key);
    }
    current_line.push_str(text);
}

fn flush_text_line(lines: &mut Vec<String>, current_line: &mut String) {
    if current_line.is_empty() {
        return;
    }
    lines.push(std::mem::take(current_line));
}

fn last_number_operand(operands: &[TextOperand], index_from_end: usize) -> Option<f64> {
    operands
        .iter()
        .rev()
        .filter_map(|operand| match operand {
            TextOperand::Number(value) => Some(*value),
            TextOperand::Text(_) => None,
        })
        .nth(index_from_end)
}

fn last_text_operand(operands: &[TextOperand]) -> Option<&str> {
    operands.iter().rev().find_map(|operand| match operand {
        TextOperand::Text(value) => Some(value.as_str()),
        TextOperand::Number(_) => None,
    })
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

#[derive(Debug, Clone, PartialEq)]
enum TextContentToken<'a> {
    Number(f64),
    Operator(&'a str),
    Text(String),
}

struct TextContentTokenizer<'a> {
    data: &'a [u8],
    index: usize,
}

impl<'a> TextContentTokenizer<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, index: 0 }
    }

    fn next(&mut self) -> Option<TextContentToken<'a>> {
        while self.index < self.data.len() {
            self.index = skip_pdf_whitespace_and_comments(self.data, self.index);
            if self.index >= self.data.len() {
                return None;
            }

            match self.data[self.index] {
                b'(' => {
                    let (bytes, next_index) = parse_pdf_literal_string(self.data, self.index)?;
                    self.index = next_index;
                    return Some(TextContentToken::Text(decode_pdf_text(&bytes)));
                }
                b'[' => {
                    let (text, next_index) = parse_pdf_text_array(self.data, self.index)?;
                    self.index = next_index;
                    return Some(TextContentToken::Text(text));
                }
                b'<' if self.data.get(self.index + 1) == Some(&b'<') => {
                    self.index = find_matching_dictionary_end(self.data, self.index)
                        .unwrap_or(self.index + 1);
                    continue;
                }
                b'<' => {
                    let (bytes, next_index) = parse_pdf_hex_string(self.data, self.index)?;
                    self.index = next_index;
                    return Some(TextContentToken::Text(decode_pdf_text(&bytes)));
                }
                b'/' => {
                    self.index = skip_pdf_name_token(self.data, self.index);
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
                return Some(TextContentToken::Number(number));
            }
            return Some(TextContentToken::Operator(token));
        }

        None
    }
}

fn single_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn pass_fail(pass: bool) -> &'static str {
    if pass {
        "PASS"
    } else {
        "FAIL"
    }
}

fn parse_pdf_text_array(data: &[u8], start: usize) -> Option<(String, usize)> {
    if data.get(start) != Some(&b'[') {
        return None;
    }

    let mut index = start + 1;
    let mut text = String::new();

    while index < data.len() {
        index = skip_pdf_whitespace_and_comments(data, index);
        match data.get(index) {
            Some(b']') => return Some((text, index + 1)),
            Some(b'(') | Some(b'<') if data.get(index + 1) != Some(&b'<') => {
                let (bytes, next_index) = parse_pdf_string_bytes(data, index)?;
                text.push_str(&decode_pdf_text(&bytes));
                index = next_index;
            }
            Some(_) => {
                let next_index = skip_pdf_value(data, index);
                index = if next_index == index {
                    index + 1
                } else {
                    next_index
                };
            }
            None => return None,
        }
    }

    None
}

fn normalize_bibliography_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn is_bibliography_heading(line: &str) -> bool {
    line.eq_ignore_ascii_case("references") || line.eq_ignore_ascii_case("bibliography")
}

fn extract_citation_label_from_line(line: &str) -> Option<String> {
    let line = line.trim_start();
    if !line.starts_with('[') {
        return None;
    }

    let closing = line.find(']')?;
    let label = normalize_bibliography_whitespace(&line[1..closing]);
    let label = label.trim().to_string();
    if label.is_empty() {
        return None;
    }

    let compact = label.chars().filter(|character| !character.is_whitespace());
    if !compact
        .clone()
        .all(|character| character.is_ascii_alphanumeric())
    {
        return None;
    }

    Some(label)
}

fn object_dictionary_bytes(object_body: &[u8]) -> &[u8] {
    find_bytes(object_body, b"stream")
        .map(|offset| &object_body[..offset])
        .unwrap_or(object_body)
}

fn find_info_object(data: &[u8]) -> Option<&[u8]> {
    let info_ref = find_trailer_dictionary(data)
        .and_then(|trailer| extract_indirect_ref_for_key(trailer, b"/Info"))
        .or_else(|| find_xref_indirect_ref(data, b"/Info"))
        .or_else(|| {
            find_catalog_object(data)
                .and_then(|catalog| extract_indirect_ref_for_key(catalog, b"/Info"))
        })?;
    find_object_by_ref(data, info_ref)
}

fn find_root_reference(data: &[u8]) -> Option<PdfIndirectRef> {
    find_trailer_dictionary(data)
        .and_then(|trailer| extract_indirect_ref_for_key(trailer, b"/Root"))
        .or_else(|| find_xref_indirect_ref(data, b"/Root"))
}

fn find_xref_indirect_ref(data: &[u8], key: &[u8]) -> Option<PdfIndirectRef> {
    let mut offset = 0usize;
    while let Some((_, object_body, next_offset)) = next_pdf_object(data, offset) {
        if object_contains_xref_type(object_body) {
            if let Some(reference) = extract_indirect_ref_for_key(object_body, key) {
                return Some(reference);
            }
        }
        offset = next_offset;
    }
    None
}

fn find_trailer_dictionary(data: &[u8]) -> Option<&[u8]> {
    let mut search_from = 0usize;
    let mut trailer = None;

    while let Some(offset) = find_bytes(&data[search_from..], b"trailer") {
        let trailer_start = search_from + offset + b"trailer".len();
        let dict_start = skip_pdf_whitespace(data, trailer_start);
        if data.get(dict_start..dict_start + 2) == Some(b"<<") {
            if let Some(dict_end) = find_matching_dictionary_end(data, dict_start) {
                trailer = Some(&data[dict_start..dict_end]);
            }
        }
        search_from = trailer_start;
    }

    trailer
}

fn extract_outline_manifest(data: &[u8], catalog_object: &[u8]) -> (usize, usize) {
    let Some(outlines_ref) = extract_indirect_ref_for_key(catalog_object, b"/Outlines") else {
        return (0, 0);
    };
    let Some(outlines_object) = find_object_by_ref(data, outlines_ref) else {
        return (0, 0);
    };
    let Some(first_ref) = extract_indirect_ref_for_key(outlines_object, b"/First") else {
        return (0, 0);
    };

    let mut visited = BTreeSet::new();
    let mut entry_count = 0usize;
    let mut max_depth = 0usize;
    collect_outline_entries(
        data,
        first_ref,
        1,
        &mut visited,
        &mut entry_count,
        &mut max_depth,
    );
    (entry_count, max_depth)
}

fn collect_outline_entries(
    data: &[u8],
    reference: PdfIndirectRef,
    depth: usize,
    visited: &mut BTreeSet<PdfIndirectRef>,
    entry_count: &mut usize,
    max_depth: &mut usize,
) {
    if !visited.insert(reference) {
        return;
    }

    let Some(object_body) = find_object_by_ref(data, reference) else {
        return;
    };

    *entry_count += 1;
    *max_depth = (*max_depth).max(depth);

    if let Some(child_ref) = extract_indirect_ref_for_key(object_body, b"/First") {
        collect_outline_entries(data, child_ref, depth + 1, visited, entry_count, max_depth);
    }
    if let Some(next_ref) = extract_indirect_ref_for_key(object_body, b"/Next") {
        collect_outline_entries(data, next_ref, depth, visited, entry_count, max_depth);
    }
}

fn count_named_destination_references(data: &[u8], catalog_object: &[u8]) -> usize {
    extract_named_destination_names(data, catalog_object)
        .into_iter()
        .filter_map(|name| normalize_destination_name(&name))
        .collect::<BTreeSet<_>>()
        .len()
}

fn extract_named_destination_names(data: &[u8], catalog_object: &[u8]) -> Vec<String> {
    collect_named_destination_names(data, catalog_object)
}

fn collect_named_destination_names(data: &[u8], catalog_object: &[u8]) -> Vec<String> {
    let mut names = BTreeSet::new();
    let mut visited = BTreeSet::new();

    if let Some(dests_ref) = extract_indirect_ref_for_key(catalog_object, b"/Dests") {
        collect_named_destination_names_from_dests_by_ref(
            data,
            dests_ref,
            &mut visited,
            &mut names,
        );
    }
    if let Some(dests_dict) = extract_inline_dictionary_for_key(catalog_object, b"/Dests") {
        collect_named_destination_names_from_dests_body(data, dests_dict, &mut visited, &mut names);
    }
    if let Some(names_ref) = extract_indirect_ref_for_key(catalog_object, b"/Names") {
        collect_named_destination_names_from_names_by_ref(
            data,
            names_ref,
            &mut visited,
            &mut names,
        );
    }
    if let Some(names_dict) = extract_inline_dictionary_for_key(catalog_object, b"/Names") {
        collect_named_destination_names_from_names_body(data, names_dict, &mut visited, &mut names);
    }

    names.into_iter().collect()
}

fn is_hyperref_auto_destination(name: &str) -> bool {
    if name == "Doc-Start" {
        return true;
    }
    if let Some(rest) = name.strip_prefix("page.") {
        return !rest.is_empty() && rest.chars().all(|character| character.is_ascii_digit());
    }
    if let Some(rest) = name.strip_prefix("section*.") {
        return !rest.is_empty() && rest.chars().all(|character| character.is_ascii_digit());
    }
    // Label aliases (`sec:...`, `fig:...`) duplicate counter-based destinations.
    if name.contains(':') {
        return true;
    }
    false
}

fn normalize_destination_name(name: &str) -> Option<String> {
    if let Some(normalized) = normalize_structural_destination_name(name) {
        return Some(normalized);
    }
    if let Some(normalized) = normalize_bibliography_destination_name(name) {
        return Some(normalized);
    }
    if is_hyperref_auto_destination(name) {
        return None;
    }
    Some(name.to_string())
}

fn normalize_structural_destination_name(name: &str) -> Option<String> {
    if let Some(number) = name.strip_prefix("section.") {
        return normalize_structural_destination_number(number);
    }
    if let Some(number) = name.strip_prefix("subsection.") {
        return normalize_structural_destination_number(number);
    }
    if let Some(number) = name.strip_prefix("subsubsection.") {
        return normalize_structural_destination_number(number);
    }
    if let Some(rest) = name.strip_prefix("section:") {
        let number = rest.split_once(' ').map_or(rest, |(number, _)| number);
        return normalize_structural_destination_number(number);
    }
    None
}

fn normalize_structural_destination_number(number: &str) -> Option<String> {
    if is_structural_destination_number(number) {
        Some(format!("heading.{number}"))
    } else {
        None
    }
}

fn normalize_bibliography_destination_name(name: &str) -> Option<String> {
    if let Some(key) = name.strip_prefix("bib:") {
        return Some(format!("citation.{key}"));
    }
    if let Some(key) = name.strip_prefix("cite.") {
        return Some(format!("citation.{key}"));
    }
    None
}

fn is_structural_destination_number(number: &str) -> bool {
    !number.is_empty()
        && number
            .split('.')
            .all(|segment| !segment.is_empty() && segment.chars().all(|c| c.is_ascii_digit()))
}

fn collect_named_destination_names_from_names_by_ref(
    data: &[u8],
    reference: PdfIndirectRef,
    visited: &mut BTreeSet<PdfIndirectRef>,
    names: &mut BTreeSet<String>,
) {
    if !visited.insert(reference) {
        return;
    }

    let Some(object_body) = find_object_by_ref(data, reference) else {
        return;
    };
    collect_named_destination_names_from_names_body(data, object_body, visited, names)
}

fn collect_named_destination_names_from_names_body(
    data: &[u8],
    object_body: &[u8],
    visited: &mut BTreeSet<PdfIndirectRef>,
    names: &mut BTreeSet<String>,
) {
    if let Some(dests_ref) = extract_indirect_ref_for_key(object_body, b"/Dests") {
        collect_name_tree_destination_names_by_ref(data, dests_ref, visited, names);
    }
    if let Some(dests_dict) = extract_inline_dictionary_for_key(object_body, b"/Dests") {
        collect_name_tree_destination_names_in_body(data, dests_dict, visited, names);
    }
}

fn collect_named_destination_names_from_dests_by_ref(
    data: &[u8],
    reference: PdfIndirectRef,
    visited: &mut BTreeSet<PdfIndirectRef>,
    names: &mut BTreeSet<String>,
) {
    if !visited.insert(reference) {
        return;
    }

    let Some(object_body) = find_object_by_ref(data, reference) else {
        return;
    };
    collect_named_destination_names_from_dests_body(data, object_body, visited, names);
}

fn collect_named_destination_names_from_dests_body(
    data: &[u8],
    object_body: &[u8],
    visited: &mut BTreeSet<PdfIndirectRef>,
    names: &mut BTreeSet<String>,
) {
    let mut handled_as_name_tree = false;

    if let Some(names_array) = extract_inline_array_for_key(object_body, b"/Names") {
        handled_as_name_tree = true;
        collect_destination_names_from_names_array(names_array, names);
    }
    let kids = extract_array_refs_for_key(object_body, b"/Kids");
    if !kids.is_empty() {
        handled_as_name_tree = true;
        for reference in kids {
            collect_name_tree_destination_names_by_ref(data, reference, visited, names);
        }
    }

    if !handled_as_name_tree {
        collect_top_level_dictionary_names(object_body, names);
    }
}

fn collect_name_tree_destination_names_by_ref(
    data: &[u8],
    reference: PdfIndirectRef,
    visited: &mut BTreeSet<PdfIndirectRef>,
    names: &mut BTreeSet<String>,
) {
    if !visited.insert(reference) {
        return;
    }

    let Some(object_body) = find_object_by_ref(data, reference) else {
        return;
    };
    collect_name_tree_destination_names_in_body(data, object_body, visited, names)
}

fn collect_name_tree_destination_names_in_body(
    data: &[u8],
    object_body: &[u8],
    visited: &mut BTreeSet<PdfIndirectRef>,
    names: &mut BTreeSet<String>,
) {
    if let Some(names_array) = extract_inline_array_for_key(object_body, b"/Names") {
        collect_destination_names_from_names_array(names_array, names);
    }

    for reference in extract_array_refs_for_key(object_body, b"/Kids") {
        collect_name_tree_destination_names_by_ref(data, reference, visited, names);
    }
}

fn collect_destination_names_from_names_array(array_body: &[u8], names: &mut BTreeSet<String>) {
    let mut index = 0usize;
    let mut value_index = 0usize;

    while index < array_body.len() {
        index = skip_pdf_whitespace_and_comments(array_body, index);
        if index >= array_body.len() {
            break;
        }

        let next_index = if value_index % 2 == 0 {
            if let Some((name, next_index)) = parse_named_destination_name(array_body, index) {
                names.insert(name);
                next_index
            } else {
                skip_pdf_value(array_body, index)
            }
        } else {
            skip_pdf_value(array_body, index)
        };

        index = if next_index == index {
            index + 1
        } else {
            next_index
        };
        value_index += 1;
    }
}

fn parse_named_destination_name(data: &[u8], start: usize) -> Option<(String, usize)> {
    if let Some((bytes, next_index)) = parse_pdf_string_bytes(data, start) {
        return Some((decode_pdf_text(&bytes), next_index));
    }

    if data.get(start) == Some(&b'/') {
        let end = skip_pdf_name_token(data, start);
        return Some((decode_pdf_name(&data[start + 1..end]), end));
    }

    None
}

fn collect_top_level_dictionary_names(object_body: &[u8], names: &mut BTreeSet<String>) {
    let object_body = object_dictionary_bytes(object_body);
    let body = strip_enclosing_dictionary(object_body);
    let mut index = 0usize;

    while index < body.len() {
        index = skip_pdf_whitespace_and_comments(body, index);
        if index >= body.len() {
            break;
        }

        if body[index] != b'/' {
            let next_index = skip_pdf_value(body, index);
            index = if next_index == index {
                index + 1
            } else {
                next_index
            };
            continue;
        }

        let key_end = skip_pdf_name_token(body, index);
        names.insert(decode_pdf_name(&body[index + 1..key_end]));
        index = skip_pdf_whitespace_and_comments(body, key_end);
        if index >= body.len() {
            break;
        }
        let next_index = skip_pdf_value(body, index);
        index = if next_index == index {
            index + 1
        } else {
            next_index
        };
    }
}

fn decode_pdf_name(data: &[u8]) -> String {
    let mut bytes = Vec::with_capacity(data.len());
    let mut index = 0usize;

    while index < data.len() {
        if data[index] == b'#' && index + 2 < data.len() {
            if let (Some(high), Some(low)) =
                (hex_value(data[index + 1]), hex_value(data[index + 2]))
            {
                bytes.push((high << 4) | low);
                index += 3;
                continue;
            }
        }

        bytes.push(data[index]);
        index += 1;
    }

    String::from_utf8_lossy(&bytes).into_owned()
}

fn find_object_in_object_streams(data: &[u8], reference: PdfIndirectRef) -> Option<&[u8]> {
    let mut offset = 0usize;
    while let Some((object_ref, object_body, next_offset)) = next_pdf_object(data, offset) {
        if object_contains_objstm_type(object_body) {
            if let Ok(stream) = extract_uncompressed_stream_slice(data, object_ref) {
                let mut stream_offset = 0usize;
                while let Some((embedded_ref, embedded_object, next_stream_offset)) =
                    next_object_stream_object(stream, stream_offset)
                {
                    if embedded_ref == reference {
                        return Some(embedded_object);
                    }
                    stream_offset = next_stream_offset;
                }
            }
        }

        offset = next_offset;
    }

    None
}

fn next_object_stream_object(
    data: &[u8],
    search_from: usize,
) -> Option<(PdfIndirectRef, &[u8], usize)> {
    let mut index = search_from;

    while index < data.len() {
        let Some(marker_offset) = data[index..].iter().position(|byte| *byte == b'%') else {
            return None;
        };
        let marker_start = index + marker_offset;
        if marker_start > 0 && !matches!(data[marker_start - 1], b'\n' | b'\r') {
            index = marker_start + 1;
            continue;
        }

        let Some(object_ref) = extract_object_stream_marker(&data[marker_start..]) else {
            index = marker_start + 1;
            continue;
        };
        let body_start = marker_start + skip_after_object_stream_marker(&data[marker_start..], 0)?;
        let body_end = find_next_object_stream_marker(data, body_start).unwrap_or(data.len());
        return Some((
            object_ref,
            trim_ascii_whitespace(&data[body_start..body_end]),
            body_end,
        ));
    }

    None
}

fn find_next_object_stream_marker(data: &[u8], search_from: usize) -> Option<usize> {
    let mut index = search_from;
    while index < data.len() {
        let Some(marker_offset) = data[index..].iter().position(|byte| *byte == b'%') else {
            return None;
        };
        let marker_start = index + marker_offset;
        if marker_start > 0 && !matches!(data[marker_start - 1], b'\n' | b'\r') {
            index = marker_start + 1;
            continue;
        }
        if extract_object_stream_marker(&data[marker_start..]).is_some() {
            return Some(marker_start);
        }
        index = marker_start + 1;
    }
    None
}

fn extract_object_stream_marker(data: &[u8]) -> Option<PdfIndirectRef> {
    if data.first() != Some(&b'%') {
        return None;
    }
    let index = skip_pdf_whitespace(data, 1);
    let object_number_end = index
        + data[index..]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .count();
    if object_number_end == index {
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
    let obj_start = skip_pdf_whitespace(data, generation_end);
    if data.get(obj_start..obj_start + 3) != Some(b"obj") {
        return None;
    }

    Some(PdfIndirectRef {
        object_number: std::str::from_utf8(&data[index..object_number_end])
            .ok()?
            .parse()
            .ok()?,
        generation: std::str::from_utf8(&data[generation_start..generation_end])
            .ok()?
            .parse()
            .ok()?,
    })
}

fn skip_after_object_stream_marker(data: &[u8], start: usize) -> Option<usize> {
    let mut index = start;
    if data.get(index) != Some(&b'%') {
        return None;
    }
    index = skip_pdf_whitespace(data, index + 1);
    index += data[index..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    index = skip_pdf_whitespace(data, index);
    index += data[index..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    index = skip_pdf_whitespace(data, index);
    if data.get(index..index + 3) != Some(b"obj") {
        return None;
    }
    index += 3;
    while matches!(data.get(index), Some(b'\r' | b'\n' | b' ' | b'\t')) {
        index += 1;
    }
    Some(index)
}

fn extract_pdf_string_for_key(object_body: &[u8], key: &[u8]) -> Option<String> {
    let object_body = object_dictionary_bytes(object_body);
    let key_start = find_key_offset(object_body, key)?;
    let value_start = skip_pdf_whitespace(object_body, key_start + key.len());
    let (bytes, _) = parse_pdf_string_bytes(object_body, value_start)?;
    Some(decode_pdf_text(&bytes))
}

fn extract_pdf_name_values_for_key(object_body: &[u8], key: &[u8]) -> Vec<String> {
    let object_body = object_dictionary_bytes(object_body);
    let mut values = Vec::new();
    let mut search_from = 0usize;

    while let Some(key_start) = find_key_offset_from(object_body, key, search_from) {
        let value_start = skip_pdf_whitespace(object_body, key_start + key.len());
        if object_body.get(value_start) == Some(&b'/') {
            let value_end = skip_pdf_name_token(object_body, value_start);
            if value_end > value_start + 1 {
                values.push(decode_pdf_name(&object_body[value_start + 1..value_end]));
                search_from = value_end;
                continue;
            }
        }
        search_from = value_start.saturating_add(1);
    }

    values
}

fn extract_inline_dictionary_for_key<'a>(object_body: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let object_body = object_dictionary_bytes(object_body);
    let key_start = find_key_offset(object_body, key)?;
    let dict_start = skip_pdf_whitespace(object_body, key_start + key.len());
    if object_body.get(dict_start..dict_start + 2) != Some(b"<<") {
        return None;
    }
    let dict_end = find_matching_dictionary_end(object_body, dict_start)?;
    Some(&object_body[dict_start..dict_end])
}

fn extract_inline_array_for_key<'a>(object_body: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    let object_body = object_dictionary_bytes(object_body);
    let key_start = find_key_offset(object_body, key)?;
    let array_start = skip_pdf_whitespace(object_body, key_start + key.len());
    if object_body.get(array_start) != Some(&b'[') {
        return None;
    }
    let array_end = find_matching_array_end(object_body, array_start)?;
    Some(&object_body[array_start + 1..array_end - 1])
}

fn find_key_offset(data: &[u8], key: &[u8]) -> Option<usize> {
    find_key_offset_from(data, key, 0)
}

fn find_key_offset_from(data: &[u8], key: &[u8], search_from: usize) -> Option<usize> {
    let mut offset = search_from;

    while let Some(found) = find_bytes(&data[offset..], key) {
        let start = offset + found;
        let boundary = start + key.len();
        let preceded_by_name_char = start > 0
            && matches!(
                data.get(start - 1),
                Some(byte) if byte.is_ascii_alphanumeric() || matches!(byte, b'#' | b'-' | b'_')
            );
        let followed_by_name_char = matches!(
            data.get(boundary),
            Some(byte) if byte.is_ascii_alphanumeric() || matches!(byte, b'#' | b'-' | b'_')
        );
        if !preceded_by_name_char && !followed_by_name_char {
            return Some(start);
        }
        offset = boundary;
    }

    None
}

fn strip_enclosing_dictionary(data: &[u8]) -> &[u8] {
    let data = skip_pdf_whitespace_slice(data);
    if data.starts_with(b"<<") && data.ends_with(b">>") && data.len() >= 4 {
        &data[2..data.len() - 2]
    } else {
        data
    }
}

fn skip_pdf_whitespace_slice(data: &[u8]) -> &[u8] {
    let start = skip_pdf_whitespace(data, 0);
    let end = data
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(start);
    &data[start..end]
}

fn trim_ascii_whitespace(data: &[u8]) -> &[u8] {
    let start = data
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(0);
    let end = data
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map(|index| index + 1)
        .unwrap_or(start);
    &data[start..end]
}

fn parse_pdf_string_bytes(data: &[u8], start: usize) -> Option<(Vec<u8>, usize)> {
    match data.get(start) {
        Some(b'(') => parse_pdf_literal_string(data, start),
        Some(b'<') if data.get(start + 1) != Some(&b'<') => parse_pdf_hex_string(data, start),
        _ => None,
    }
}

fn parse_pdf_literal_string(data: &[u8], start: usize) -> Option<(Vec<u8>, usize)> {
    let mut index = start + 1;
    let mut depth = 1usize;
    let mut bytes = Vec::new();
    let mut escaped = false;

    while index < data.len() {
        let byte = data[index];
        index += 1;

        if escaped {
            match byte {
                b'n' => bytes.push(b'\n'),
                b'r' => bytes.push(b'\r'),
                b't' => bytes.push(b'\t'),
                b'b' => bytes.push(0x08),
                b'f' => bytes.push(0x0c),
                b'(' | b')' | b'\\' => bytes.push(byte),
                b'\r' => {
                    if data.get(index) == Some(&b'\n') {
                        index += 1;
                    }
                }
                b'\n' => {}
                b'0'..=b'7' => {
                    let mut value = (byte - b'0') as u16;
                    let mut consumed = 0usize;
                    while consumed < 2 {
                        let Some(next) = data.get(index) else {
                            break;
                        };
                        if !(b'0'..=b'7').contains(next) {
                            break;
                        }
                        value = value * 8 + (next - b'0') as u16;
                        index += 1;
                        consumed += 1;
                    }
                    bytes.push(value as u8);
                }
                _ => bytes.push(byte),
            }
            escaped = false;
            continue;
        }

        match byte {
            b'\\' => escaped = true,
            b'(' => {
                depth += 1;
                bytes.push(byte);
            }
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some((bytes, index));
                }
                bytes.push(byte);
            }
            _ => bytes.push(byte),
        }
    }

    None
}

fn parse_pdf_hex_string(data: &[u8], start: usize) -> Option<(Vec<u8>, usize)> {
    let mut index = start + 1;
    let mut hex_digits = Vec::new();

    while index < data.len() {
        match data[index] {
            b'>' => {
                index += 1;
                break;
            }
            byte if byte.is_ascii_whitespace() => index += 1,
            byte => {
                hex_digits.push(byte);
                index += 1;
            }
        }
    }

    if hex_digits.len() % 2 != 0 {
        hex_digits.push(b'0');
    }

    let mut bytes = Vec::with_capacity(hex_digits.len() / 2);
    for pair in hex_digits.chunks(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }

    Some((bytes, index))
}

fn decode_pdf_text(bytes: &[u8]) -> String {
    if bytes.len() >= 2 {
        if bytes.starts_with(&[0xfe, 0xff]) {
            let code_units = bytes[2..]
                .chunks(2)
                .filter(|chunk| chunk.len() == 2)
                .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>();
            return String::from_utf16_lossy(&code_units);
        }
        if bytes.starts_with(&[0xff, 0xfe]) {
            let code_units = bytes[2..]
                .chunks(2)
                .filter(|chunk| chunk.len() == 2)
                .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
                .collect::<Vec<_>>();
            return String::from_utf16_lossy(&code_units);
        }
    }

    String::from_utf8_lossy(bytes).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn skip_pdf_whitespace_and_comments(data: &[u8], mut index: usize) -> usize {
    loop {
        while matches!(data.get(index), Some(byte) if byte.is_ascii_whitespace()) {
            index += 1;
        }
        if data.get(index) == Some(&b'%') {
            while index < data.len() && !matches!(data[index], b'\r' | b'\n') {
                index += 1;
            }
            continue;
        }
        return index;
    }
}

fn skip_pdf_value(data: &[u8], index: usize) -> usize {
    let index = skip_pdf_whitespace_and_comments(data, index);
    match data.get(index) {
        Some(b'(') => skip_pdf_literal_string_value(data, index),
        Some(b'[') => find_matching_array_end(data, index).unwrap_or(index),
        Some(b'<') if data.get(index + 1) == Some(&b'<') => {
            find_matching_dictionary_end(data, index).unwrap_or(index)
        }
        Some(b'<') => skip_pdf_hex_string_value(data, index),
        Some(b'/') => skip_pdf_name_token(data, index),
        Some(_) => skip_pdf_primitive_value(data, index),
        None => index,
    }
}

fn skip_pdf_name_token(data: &[u8], mut index: usize) -> usize {
    if data.get(index) != Some(&b'/') {
        return index;
    }
    index += 1;
    while index < data.len()
        && !data[index].is_ascii_whitespace()
        && !matches!(
            data[index],
            b'(' | b')' | b'[' | b']' | b'<' | b'>' | b'/' | b'%'
        )
    {
        index += 1;
    }
    index
}

fn skip_pdf_primitive_value(data: &[u8], index: usize) -> usize {
    let first_end = skip_pdf_token(data, index);
    let second_start = skip_pdf_whitespace_and_comments(data, first_end);
    let second_end = skip_pdf_token(data, second_start);
    let marker = skip_pdf_whitespace_and_comments(data, second_end);

    if second_start > first_end
        && data
            .get(index..first_end)
            .is_some_and(|token| token.iter().all(|byte| byte.is_ascii_digit()))
        && data
            .get(second_start..second_end)
            .is_some_and(|token| token.iter().all(|byte| byte.is_ascii_digit()))
        && data.get(marker) == Some(&b'R')
    {
        return marker + 1;
    }

    first_end
}

fn skip_pdf_token(data: &[u8], mut index: usize) -> usize {
    while index < data.len()
        && !data[index].is_ascii_whitespace()
        && !matches!(
            data[index],
            b'(' | b')' | b'[' | b']' | b'<' | b'>' | b'/' | b'%'
        )
    {
        index += 1;
    }
    index
}

fn skip_pdf_literal_string_value(data: &[u8], index: usize) -> usize {
    parse_pdf_literal_string(data, index)
        .map(|(_, next_index)| next_index)
        .unwrap_or(index)
}

fn skip_pdf_hex_string_value(data: &[u8], index: usize) -> usize {
    parse_pdf_hex_string(data, index)
        .map(|(_, next_index)| next_index)
        .unwrap_or(index)
}

fn find_matching_array_end(data: &[u8], start: usize) -> Option<usize> {
    find_matching_delimited_end(data, start, b"[", b"]")
}

fn find_matching_dictionary_end(data: &[u8], start: usize) -> Option<usize> {
    find_matching_delimited_end(data, start, b"<<", b">>")
}

fn find_matching_delimited_end(
    data: &[u8],
    start: usize,
    open: &[u8],
    close: &[u8],
) -> Option<usize> {
    let mut index = start;
    let mut depth = 0usize;

    while index < data.len() {
        if data.get(index..index + open.len()) == Some(open) {
            depth += 1;
            index += open.len();
            continue;
        }
        if data.get(index..index + close.len()) == Some(close) {
            depth = depth.saturating_sub(1);
            index += close.len();
            if depth == 0 {
                return Some(index);
            }
            continue;
        }

        match data[index] {
            b'(' => index = skip_pdf_literal_string_value(data, index),
            b'[' => index = find_matching_array_end(data, index).unwrap_or(index + 1),
            b'<' if data.get(index + 1) == Some(&b'<') => {
                index = find_matching_dictionary_end(data, index).unwrap_or(index + 1)
            }
            b'<' => index = skip_pdf_hex_string_value(data, index),
            b'%' => index = skip_pdf_whitespace_and_comments(data, index),
            _ => index += 1,
        }
    }

    None
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
    use std::collections::BTreeSet;

    use super::{
        compute_bibliography_parity_score, compute_embedded_assets_parity_score,
        compute_navigation_parity_score, compute_parity_score, compute_tikz_parity_score,
        extract_bibliography_manifest, extract_embedded_assets_manifest, extract_graphics_ops,
        extract_line_y_positions, extract_named_destination_names, extract_navigation_manifest,
        extract_pdf_page_count, find_catalog_object, format_bibliography_parity_summary,
        format_embedded_assets_parity_summary, format_navigation_parity_summary,
        format_parity_summary, format_tikz_parity_summary, is_hyperref_auto_destination,
        BibliographyManifest, BibliographyParityResult, BibliographyParityScore,
        EmbeddedAssetsManifest, EmbeddedAssetsParityResult, EmbeddedAssetsParityScore, GraphicsOp,
        NavigationManifest, NavigationParityResult, NavigationParityScore, ParityResult,
        ParityScore, TikzParityResult, TikzParityScore,
    };

    const MINIMAL_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /ProcSet [/PDF] >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 18 >>\nstream\n0 0 m\n200 100 l\nS\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const NESTED_PAGE_TREE_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 4 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [5 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Pages /Kids [6 0 R 7 0 R] /Count 2 >>\nendobj\n4 0 obj\n<< /Type /Pages /Kids [2 0 R 3 0 R] /Count 3 >>\nendobj\n5 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] >>\nendobj\n6 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 200 100] >>\nendobj\n7 0 obj\n<< /Type /Page /Parent 3 0 R /MediaBox [0 0 200 100] >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const LINE_POSITIONS_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 102 >>\nstream\nBT\n/F1 12 Tf\n72 720 Td\n(Hello) Tj\n0 -18 Td\n(World) Tj\n1 0 0 1 72 650 Tm\n(Again) Tj\nET\nendstream\nendobj\n5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const TWO_PAGE_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 5 0 R >>\nendobj\n4 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>\nendobj\n5 0 obj\n<< /Length 37 >>\nstream\nBT\n/F1 12 Tf\n72 720 Td\n(Page1) Tj\nET\nendstream\nendobj\n6 0 obj\n<< /Length 37 >>\nstream\nBT\n/F1 12 Tf\n72 700 Td\n(Page2) Tj\nET\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const NAVIGATION_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R /Dests 11 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [5 0 R 6 0 R] /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 19 >>\nstream\nBT\n(Hello) Tj\nET\nendstream\nendobj\n5 0 obj\n<< /Type /Annot /Subtype /Link /A << /S /GoTo /D /intro >> >>\nendobj\n6 0 obj\n<< /Type /Annot /Subtype /Link /Dest /deep >>\nendobj\n7 0 obj\n<< /Type /Outlines /First 8 0 R /Last 8 0 R /Count 2 >>\nendobj\n8 0 obj\n<< /Title (Chapter 1) /Parent 7 0 R /First 9 0 R /Last 9 0 R /Dest /intro >>\nendobj\n9 0 obj\n<< /Title (Section 1) /Parent 8 0 R /Dest /deep >>\nendobj\n11 0 obj\n<< /Doc-Start [3 0 R /Fit] /page.1 [3 0 R /Fit] /section*.1 [3 0 R /Fit] /intro [3 0 R /Fit] /deep [3 0 R /Fit] >>\nendobj\n12 0 obj\n<< /Title (Navigation Test) /Author <416c696365> >>\nendobj\ntrailer\n<< /Root 1 0 R /Info 12 0 R >>\n%%EOF\n";
    const NAMES_TREE_NAVIGATION_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names << /Dests 11 0 R >> >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 19 >>\nstream\nBT\n(Hello) Tj\nET\nendstream\nendobj\n11 0 obj\n<< /Kids [12 0 R 13 0 R] >>\nendobj\n12 0 obj\n<< /Names [(Doc-Start) [3 0 R /Fit] (page.1) [3 0 R /Fit] (intro) [3 0 R /Fit]] >>\nendobj\n13 0 obj\n<< /Names [(section*.1) [3 0 R /Fit] (deep) [3 0 R /Fit]] >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const STRUCTURAL_ALIAS_NAVIGATION_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names << /Dests 11 0 R >> >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 19 >>\nstream\nBT\n(Hello) Tj\nET\nendstream\nendobj\n11 0 obj\n<< /Names [(sec:intro) [3 0 R /Fit] (section:1 Intro) [3 0 R /Fit] (sec:scope) [3 0 R /Fit] (section:1.1 Scope) [3 0 R /Fit] (bib:knuth) [3 0 R /Fit]] >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const STRUCTURAL_COUNTER_NAVIGATION_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Names << /Dests 11 0 R >> >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 19 >>\nstream\nBT\n(Hello) Tj\nET\nendstream\nendobj\n11 0 obj\n<< /Names [(Doc-Start) [3 0 R /Fit] (page.1) [3 0 R /Fit] (section.1) [3 0 R /Fit] (subsection.1.1) [3 0 R /Fit] (cite.knuth) [3 0 R /Fit]] >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";

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

    fn navigation_pdf_with_metadata(title: Option<&str>, author: Option<&str>) -> Vec<u8> {
        let mut objects = vec![
            "1 0 obj\n<< /Type /Catalog /Pages 2 0 R /Outlines 7 0 R /Dests 11 0 R >>\nendobj\n"
                .to_string(),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_string(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Annots [5 0 R 6 0 R] /Contents 4 0 R >>\nendobj\n".to_string(),
            "4 0 obj\n<< /Length 19 >>\nstream\nBT\n(Hello) Tj\nET\nendstream\nendobj\n"
                .to_string(),
            "5 0 obj\n<< /Type /Annot /Subtype /Link /A << /S /GoTo /D /intro >> >>\nendobj\n"
                .to_string(),
            "6 0 obj\n<< /Type /Annot /Subtype /Link /Dest /deep >>\nendobj\n".to_string(),
            "7 0 obj\n<< /Type /Outlines /First 8 0 R /Last 8 0 R /Count 2 >>\nendobj\n"
                .to_string(),
            "8 0 obj\n<< /Title (Chapter 1) /Parent 7 0 R /First 9 0 R /Last 9 0 R /Dest /intro >>\nendobj\n".to_string(),
            "9 0 obj\n<< /Title (Section 1) /Parent 8 0 R /Dest /deep >>\nendobj\n"
                .to_string(),
            "11 0 obj\n<< /Doc-Start [3 0 R /Fit] /page.1 [3 0 R /Fit] /section*.1 [3 0 R /Fit] /intro [3 0 R /Fit] /deep [3 0 R /Fit] >>\nendobj\n".to_string(),
        ];

        let trailer = if title.is_some() || author.is_some() {
            objects.push(format!(
                "12 0 obj\n<< /Title ({}) /Author ({}) >>\nendobj\n",
                title.unwrap_or(""),
                author.unwrap_or("")
            ));
            "<< /Root 1 0 R /Info 12 0 R >>"
        } else {
            "<< /Root 1 0 R >>"
        };

        format!(
            "%PDF-1.4\n{}trailer\n{}\n%%EOF\n",
            objects.join(""),
            trailer
        )
        .into_bytes()
    }

    fn embedded_assets_pdf(
        font_names: &[&str],
        image_xobject_count: usize,
        form_xobject_count: usize,
        page_count: usize,
    ) -> Vec<u8> {
        let mut objects = Vec::new();
        objects.push("1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n".to_string());

        let page_object_numbers = (0..page_count).map(|index| 3 + index).collect::<Vec<_>>();
        let kids = page_object_numbers
            .iter()
            .map(|number| format!("{number} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        objects.push(format!(
            "2 0 obj\n<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n",
            kids, page_count
        ));

        let mut next_object_number = 3 + page_count;
        for page_object_number in page_object_numbers {
            objects.push(format!(
                "{page_object_number} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents {next_object_number} 0 R >>\nendobj\n"
            ));
            objects.push(format!(
                "{next_object_number} 0 obj\n<< /Length 19 >>\nstream\nBT\n(Hello) Tj\nET\nendstream\nendobj\n"
            ));
            next_object_number += 1;
        }

        for font_name in font_names {
            objects.push(format!(
                "{next_object_number} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /{} >>\nendobj\n",
                font_name
            ));
            next_object_number += 1;
        }

        for _ in 0..image_xobject_count {
            objects.push(format!(
                "{next_object_number} 0 obj\n<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Length 1 >>\nstream\n0\nendstream\nendobj\n"
            ));
            next_object_number += 1;
        }

        for _ in 0..form_xobject_count {
            objects.push(format!(
                "{next_object_number} 0 obj\n<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] /Resources << >> /Length 0 >>\nstream\nendstream\nendobj\n"
            ));
            next_object_number += 1;
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
    fn extract_pdf_page_count_resolves_nested_page_tree() {
        assert_eq!(extract_pdf_page_count(NESTED_PAGE_TREE_PDF).unwrap(), 3);
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
    fn extract_navigation_manifest_reads_minimal_pdf() {
        let manifest = extract_navigation_manifest(NAVIGATION_PDF).unwrap();

        assert_eq!(manifest.annotations_per_page, vec![2]);
        assert_eq!(manifest.named_destination_count, 2);
        assert_eq!(manifest.outline_entry_count, 2);
        assert_eq!(manifest.outline_max_depth, 2);
        assert_eq!(manifest.metadata_title.as_deref(), Some("Navigation Test"));
        assert_eq!(manifest.metadata_author.as_deref(), Some("Alice"));
    }

    #[test]
    fn compute_navigation_parity_score_matches_identical_pdfs() {
        let score = compute_navigation_parity_score(NAVIGATION_PDF, NAVIGATION_PDF).unwrap();

        assert!(score.annotations_match);
        assert!(score.destinations_match);
        assert!(score.outlines_match);
        assert!(score.metadata_title_match);
        assert!(score.metadata_author_match);
        assert!(score.passes_req_nf_007());
        assert!(score.failure_reasons().is_empty());
        assert!(score.pass);
    }

    #[test]
    fn extract_named_destination_names_reads_names_tree_pairs() {
        let catalog = find_catalog_object(NAMES_TREE_NAVIGATION_PDF).unwrap();
        let mut names = extract_named_destination_names(NAMES_TREE_NAVIGATION_PDF, catalog);

        names.sort();
        assert_eq!(
            names,
            vec![
                "Doc-Start".to_string(),
                "deep".to_string(),
                "intro".to_string(),
                "page.1".to_string(),
                "section*.1".to_string(),
            ]
        );
    }

    #[test]
    fn extract_navigation_manifest_filters_hyperref_auto_destinations_in_names_tree() {
        let manifest = extract_navigation_manifest(NAMES_TREE_NAVIGATION_PDF).unwrap();

        assert_eq!(manifest.annotations_per_page, vec![0]);
        assert_eq!(manifest.named_destination_count, 2);
        assert_eq!(manifest.outline_entry_count, 0);
        assert_eq!(manifest.outline_max_depth, 0);
        assert_eq!(manifest.metadata_title, None);
        assert_eq!(manifest.metadata_author, None);
    }

    #[test]
    fn compute_navigation_parity_score_normalizes_structural_destinations_and_filters_aliases() {
        let score = compute_navigation_parity_score(
            STRUCTURAL_ALIAS_NAVIGATION_PDF,
            STRUCTURAL_COUNTER_NAVIGATION_PDF,
        )
        .unwrap();

        assert_eq!(score.ferritex_manifest.named_destination_count, 3);
        assert_eq!(score.reference_manifest.named_destination_count, 3);
        assert!(score.destinations_match);
        assert!(score.pass);
    }

    #[test]
    fn is_hyperref_auto_destination_matches_supported_patterns() {
        assert!(is_hyperref_auto_destination("Doc-Start"));
        assert!(is_hyperref_auto_destination("page.1"));
        assert!(is_hyperref_auto_destination("page.42"));
        assert!(is_hyperref_auto_destination("section*.1"));
        assert!(is_hyperref_auto_destination("section*.99"));
        assert!(is_hyperref_auto_destination("sec:first"));
        assert!(is_hyperref_auto_destination("fig:1"));
        assert!(is_hyperref_auto_destination("eq:main"));
        assert!(!is_hyperref_auto_destination("page."));
        assert!(!is_hyperref_auto_destination("page.one"));
        assert!(!is_hyperref_auto_destination("section.1"));
        assert!(!is_hyperref_auto_destination("section*.x"));
        assert!(!is_hyperref_auto_destination("intro"));
    }

    #[test]
    fn compute_navigation_parity_score_treats_missing_and_empty_metadata_as_equal() {
        let ferritex_pdf = navigation_pdf_with_metadata(None, None);
        let reference_pdf = navigation_pdf_with_metadata(Some(""), Some(""));

        let score = compute_navigation_parity_score(&ferritex_pdf, &reference_pdf).unwrap();

        assert!(score.annotations_match);
        assert!(score.destinations_match);
        assert!(score.outlines_match);
        assert!(score.metadata_title_match);
        assert!(score.metadata_author_match);
        assert!(score.pass);
    }

    #[test]
    fn extract_bibliography_manifest_reads_reference_section_labels() {
        let pdf = pdf_with_streams(&[
            "BT /F1 12 Tf 72 720 Td (Intro [1] text.) Tj 0 -18 Td (References) Tj 0 -18 Td [([1])-1200(Donald)-300(Knuth.)] TJ 0 -18 Td [([Knu)40(84])-900(Another)-300(Entry)] TJ ET",
        ]);

        let manifest = extract_bibliography_manifest(&pdf).unwrap();

        assert_eq!(
            manifest.citation_labels,
            vec!["1".to_string(), "Knu84".to_string()]
        );
        assert_eq!(manifest.entry_count, 2);
    }

    #[test]
    fn compute_bibliography_parity_score_detects_label_order_mismatch() {
        let ferritex_pdf = pdf_with_streams(&[
            "BT 72 720 Td (References) Tj 0 -18 Td [([1])-500(Entry)] TJ 0 -18 Td [([2])-500(Entry)] TJ ET",
        ]);
        let reference_pdf = pdf_with_streams(&[
            "BT 72 720 Td (References) Tj 0 -18 Td [([2])-500(Entry)] TJ 0 -18 Td [([1])-500(Entry)] TJ ET",
        ]);

        let score = compute_bibliography_parity_score(&ferritex_pdf, &reference_pdf).unwrap();

        assert!(score.entry_count_match);
        assert!(!score.labels_match);
        assert!(!score.passes_req_nf_007());
        assert_eq!(
            score.failure_reasons(),
            vec![
                "citation labels mismatch: ferritex=[\"1\", \"2\"], reference=[\"2\", \"1\"]"
                    .to_string()
            ]
        );
        assert!(!score.pass);
    }

    #[test]
    fn extract_embedded_assets_manifest_reads_fonts_and_xobject_counts() {
        let pdf = embedded_assets_pdf(&["ABCDEE+CMR10", "Helvetica"], 2, 1, 1);

        let manifest = extract_embedded_assets_manifest(&pdf).unwrap();

        assert_eq!(
            manifest.font_names,
            BTreeSet::from(["ABCDEE+CMR10".to_string(), "Helvetica".to_string(),])
        );
        assert_eq!(manifest.image_xobject_count, 2);
        assert_eq!(manifest.form_xobject_count, 1);
        assert_eq!(manifest.page_count, 1);
    }

    #[test]
    fn compute_embedded_assets_parity_score_detects_manifest_mismatch() {
        let ferritex_pdf = embedded_assets_pdf(&["Helvetica"], 1, 0, 1);
        let reference_pdf = embedded_assets_pdf(&["ABCDEE+CMR10", "Helvetica"], 2, 1, 2);

        let score = compute_embedded_assets_parity_score(&ferritex_pdf, &reference_pdf).unwrap();

        assert!(!score.font_set_match);
        assert!(!score.image_count_match);
        assert!(!score.form_count_match);
        assert!(!score.page_count_match);
        assert!(!score.passes_req_nf_007());
        assert_eq!(
            score.failure_reasons(),
            vec![
                "font set mismatch: ferritex={\"Helvetica\"}, reference={\"CMR10\", \"Helvetica\"}"
                    .to_string(),
                "image XObject count mismatch: ferritex=1, reference=2".to_string(),
                "form XObject count mismatch: ferritex=0, reference=1".to_string(),
                "page count mismatch: ferritex=1, reference=2".to_string(),
            ]
        );
        assert!(!score.pass);
    }

    #[test]
    fn compute_embedded_assets_parity_score_normalizes_subset_prefixes() {
        let ferritex_pdf = embedded_assets_pdf(&["FERRTX+CMR10", "FERRTX+CMBX12"], 1, 0, 1);
        let reference_pdf = embedded_assets_pdf(&["ABCDEE+CMR10", "QQQQQQ+CMBX12"], 1, 0, 1);

        let score = compute_embedded_assets_parity_score(&ferritex_pdf, &reference_pdf).unwrap();

        assert!(score.font_set_match);
        assert_eq!(
            score.ferritex_manifest.font_names,
            BTreeSet::from(["CMBX12".to_string(), "CMR10".to_string()])
        );
        assert_eq!(
            score.reference_manifest.font_names,
            BTreeSet::from(["CMBX12".to_string(), "CMR10".to_string()])
        );
        assert!(score.pass);
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

    #[test]
    fn format_navigation_parity_summary_renders_table() {
        let output = format_navigation_parity_summary(&[
            NavigationParityResult {
                document_name: "hyperref_basic".to_string(),
                score: Some(NavigationParityScore {
                    annotations_match: true,
                    destinations_match: true,
                    outlines_match: true,
                    metadata_title_match: true,
                    metadata_author_match: true,
                    ferritex_manifest: NavigationManifest {
                        annotations_per_page: vec![1],
                        named_destination_count: 3,
                        outline_entry_count: 2,
                        outline_max_depth: 2,
                        metadata_title: Some("Doc".to_string()),
                        metadata_author: Some("Alice".to_string()),
                    },
                    reference_manifest: NavigationManifest {
                        annotations_per_page: vec![1],
                        named_destination_count: 3,
                        outline_entry_count: 2,
                        outline_max_depth: 2,
                        metadata_title: Some("Doc".to_string()),
                        metadata_author: Some("Alice".to_string()),
                    },
                    pass: true,
                }),
                error: None,
            },
            NavigationParityResult {
                document_name: "pdf_metadata".to_string(),
                score: Some(NavigationParityScore {
                    annotations_match: true,
                    destinations_match: false,
                    outlines_match: true,
                    metadata_title_match: false,
                    metadata_author_match: true,
                    ferritex_manifest: NavigationManifest {
                        annotations_per_page: vec![1],
                        named_destination_count: 2,
                        outline_entry_count: 1,
                        outline_max_depth: 1,
                        metadata_title: Some("Ferritex".to_string()),
                        metadata_author: Some("Alice".to_string()),
                    },
                    reference_manifest: NavigationManifest {
                        annotations_per_page: vec![1],
                        named_destination_count: 3,
                        outline_entry_count: 1,
                        outline_max_depth: 1,
                        metadata_title: Some("Reference".to_string()),
                        metadata_author: Some("Alice".to_string()),
                    },
                    pass: false,
                }),
                error: None,
            },
            NavigationParityResult {
                document_name: "mixed_navigation".to_string(),
                score: None,
                error: Some("missing reference PDF".to_string()),
            },
        ]);

        assert!(output.contains("REQ-NF-007 Navigation Parity Summary"));
        assert!(output.contains("Document"));
        assert!(output.contains("Annots"));
        assert!(output.contains("Dests"));
        assert!(output.contains("Outlines"));
        assert!(output.contains("Meta"));
        assert!(output.contains("hyperref_basic"));
        assert!(output.contains("pdf_metadata"));
        assert!(output.contains("mixed_navigation"));
        assert!(output.contains("ERROR: missing reference PDF"));
        assert!(output.contains("Total: 2 measured, 1 pass, 1 fail, 1 error"));
        assert!(output.contains("Failure details:"));
        assert!(output.contains("named destination count mismatch: ferritex=2, reference=3"));
        assert!(output.contains(
            "metadata Title mismatch: ferritex=Some(\"Ferritex\"), reference=Some(\"Reference\")"
        ));
    }

    #[test]
    fn format_bibliography_parity_summary_renders_table() {
        let output = format_bibliography_parity_summary(&[
            BibliographyParityResult {
                document_name: "single_cite".to_string(),
                score: Some(BibliographyParityScore {
                    entry_count_match: true,
                    labels_match: true,
                    ferritex_manifest: BibliographyManifest {
                        entry_count: 1,
                        citation_labels: vec!["1".to_string()],
                    },
                    reference_manifest: BibliographyManifest {
                        entry_count: 1,
                        citation_labels: vec!["1".to_string()],
                    },
                    pass: true,
                }),
                error: None,
            },
            BibliographyParityResult {
                document_name: "custom_labels".to_string(),
                score: Some(BibliographyParityScore {
                    entry_count_match: true,
                    labels_match: false,
                    ferritex_manifest: BibliographyManifest {
                        entry_count: 1,
                        citation_labels: vec!["Knu84".to_string()],
                    },
                    reference_manifest: BibliographyManifest {
                        entry_count: 1,
                        citation_labels: vec!["Knuth84".to_string()],
                    },
                    pass: false,
                }),
                error: None,
            },
            BibliographyParityResult {
                document_name: "multi_cite".to_string(),
                score: None,
                error: Some("missing reference PDF".to_string()),
            },
        ]);

        assert!(output.contains("REQ-NF-007 Bibliography Parity Summary"));
        assert!(output.contains("Document"));
        assert!(output.contains("Entries"));
        assert!(output.contains("Labels"));
        assert!(output.contains("single_cite"));
        assert!(output.contains("custom_labels"));
        assert!(output.contains("multi_cite"));
        assert!(output.contains("ERROR: missing reference PDF"));
        assert!(output.contains("Total: 2 measured, 1 pass, 1 fail, 1 error"));
        assert!(output.contains("Failure details:"));
        assert!(output
            .contains("citation labels mismatch: ferritex=[\"Knu84\"], reference=[\"Knuth84\"]"));
    }

    #[test]
    fn format_embedded_assets_parity_summary_renders_table() {
        let output = format_embedded_assets_parity_summary(&[
            EmbeddedAssetsParityResult {
                document_name: "png_embed".to_string(),
                score: Some(EmbeddedAssetsParityScore {
                    font_set_match: true,
                    image_count_match: true,
                    form_count_match: true,
                    page_count_match: true,
                    ferritex_manifest: EmbeddedAssetsManifest {
                        font_names: BTreeSet::from(["Helvetica".to_string()]),
                        image_xobject_count: 1,
                        form_xobject_count: 0,
                        page_count: 1,
                    },
                    reference_manifest: EmbeddedAssetsManifest {
                        font_names: BTreeSet::from(["Helvetica".to_string()]),
                        image_xobject_count: 1,
                        form_xobject_count: 0,
                        page_count: 1,
                    },
                    pass: true,
                }),
                error: None,
            },
            EmbeddedAssetsParityResult {
                document_name: "mixed_embeds".to_string(),
                score: Some(EmbeddedAssetsParityScore {
                    font_set_match: true,
                    image_count_match: false,
                    form_count_match: true,
                    page_count_match: true,
                    ferritex_manifest: EmbeddedAssetsManifest {
                        font_names: BTreeSet::from(["Helvetica".to_string()]),
                        image_xobject_count: 1,
                        form_xobject_count: 1,
                        page_count: 1,
                    },
                    reference_manifest: EmbeddedAssetsManifest {
                        font_names: BTreeSet::from(["Helvetica".to_string()]),
                        image_xobject_count: 2,
                        form_xobject_count: 1,
                        page_count: 1,
                    },
                    pass: false,
                }),
                error: None,
            },
            EmbeddedAssetsParityResult {
                document_name: "pdf_embed".to_string(),
                score: None,
                error: Some("missing reference PDF".to_string()),
            },
        ]);

        assert!(output.contains("REQ-NF-007 Embedded Assets Parity Summary"));
        assert!(output.contains("Document"));
        assert!(output.contains("Fonts"));
        assert!(output.contains("Images"));
        assert!(output.contains("Forms"));
        assert!(output.contains("Pages"));
        assert!(output.contains("png_embed"));
        assert!(output.contains("mixed_embeds"));
        assert!(output.contains("pdf_embed"));
        assert!(output.contains("ERROR: missing reference PDF"));
        assert!(output.contains("Total: 2 measured, 1 pass, 1 fail, 1 error"));
        assert!(output.contains("Failure details:"));
        assert!(output.contains("image XObject count mismatch: ferritex=1, reference=2"));
    }
}
