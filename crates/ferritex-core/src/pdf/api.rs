use std::collections::{hash_map::DefaultHasher, BTreeMap, BTreeSet};
use std::hash::{Hash, Hasher};
use std::thread;

use serde::{Deserialize, Serialize};

use crate::compilation::{DocumentPartitionPlan, LinkStyle};
use crate::graphics::api::{
    ArrowSpec, Color, DashPattern, GraphicGroup, GraphicNode, GraphicsScene, ImageColorSpace,
    LineCap, LineJoin, PathSegment, Point,
};
use crate::kernel::api::DimensionValue;
use crate::typesetting::api::{
    FloatPlacement, TextLine, TypesetDocument, TypesetOutline, TypesetPage, FOOTNOTE_MARKER_END,
    FOOTNOTE_MARKER_START,
};
use crate::typesetting::math_layout::{
    SUBSCRIPT_END_MARKER, SUBSCRIPT_START_MARKER, SUPERSCRIPT_END_MARKER, SUPERSCRIPT_START_MARKER,
};

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const LEFT_MARGIN_PT: i64 = 72;
const LINK_CHAR_WIDTH_PT: i64 = 6;
const LINK_HEIGHT_PT: i64 = 12;
const LINK_DESCENT_PT: i64 = 2;
const SUPERSCRIPT_RISE_PT: i64 = 4;
const FOOTNOTE_SUPERSCRIPT_RISE_PT: i64 = 3;
const SUBSCRIPT_DROP_PT: i64 = 3;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PdfDocument {
    pub bytes: Vec<u8>,
    pub page_count: usize,
    pub total_lines: usize,
    pub encoding_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedPdfDocument {
    pub document: PdfDocument,
    pub page_payloads: Vec<PageRenderPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FontResource {
    /// A bare Type1 reference (e.g., Helvetica) - no embedding
    BuiltinType1 { base_font: String },
    /// Adobe Symbol Type1 font reference - uses the font's built-in Symbol encoding
    /// (Greek letters and math glyphs); not a WinAnsi encoded text font.
    SymbolBuiltin,
    /// An embedded Type1 font backed by a PFB program
    EmbeddedType1 {
        base_font: String,
        ascii_length: usize,
        binary_length: usize,
        trailer_length: usize,
        font_program: Vec<u8>,
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
        /// PDF font descriptor flags
        flags: u32,
    },
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFilter {
    DCTDecode,
    FlateDecode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfImageXObject {
    pub object_id: usize,
    pub width: u32,
    pub height: u32,
    pub color_space: ImageColorSpace,
    pub bits_per_component: u8,
    pub data: Vec<u8>,
    pub filter: ImageFilter,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PdfFormXObject {
    pub object_id: usize,
    pub media_box: [f64; 4],
    pub data: Vec<u8>,
    pub resources_dict: Option<String>,
}

impl Eq for PdfFormXObject {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedImage {
    pub xobject_index: usize,
    pub x: DimensionValue,
    pub y: DimensionValue,
    pub display_width: DimensionValue,
    pub display_height: DimensionValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedFormXObject {
    pub xobject_index: usize,
    pub x: DimensionValue,
    pub y: DimensionValue,
    pub display_width: DimensionValue,
    pub display_height: DimensionValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfRenderer {
    fonts: Vec<FontResource>,
    images: Vec<PdfImageXObject>,
    page_images: Vec<Vec<PlacedImage>>,
    form_xobjects: Vec<PdfFormXObject>,
    page_form_xobjects: Vec<Vec<PlacedFormXObject>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PdfLinkAnnotation {
    pub object_id: usize,
    pub target: PdfLinkTarget,
    pub x_start: DimensionValue,
    pub x_end: DimensionValue,
    pub y_bottom: DimensionValue,
    pub y_top: DimensionValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PdfLinkTarget {
    Uri(String),
    InternalDestination(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutlineObject {
    object_id: usize,
    body: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct OutlineBuildResult {
    objects: Vec<OutlineObject>,
    root_child_object_ids: Vec<usize>,
}

impl PdfRenderer {
    pub fn new() -> Self {
        Self {
            fonts: default_font_resources(),
            images: Vec::new(),
            page_images: Vec::new(),
            form_xobjects: Vec::new(),
            page_form_xobjects: Vec::new(),
        }
    }

    pub fn with_fonts(fonts: Vec<FontResource>) -> Self {
        Self {
            fonts: if fonts.is_empty() {
                default_font_resources()
            } else {
                fonts
            },
            images: Vec::new(),
            page_images: Vec::new(),
            form_xobjects: Vec::new(),
            page_form_xobjects: Vec::new(),
        }
    }

    pub fn with_images(
        mut self,
        images: Vec<PdfImageXObject>,
        page_images: Vec<Vec<PlacedImage>>,
    ) -> Self {
        self.images = images;
        self.page_images = page_images;
        self
    }

    pub fn with_form_xobjects(
        mut self,
        form_xobjects: Vec<PdfFormXObject>,
        page_form_xobjects: Vec<Vec<PlacedFormXObject>>,
    ) -> Self {
        self.form_xobjects = form_xobjects;
        self.page_form_xobjects = page_form_xobjects;
        self
    }

    pub fn render(&self, document: &TypesetDocument) -> PdfDocument {
        self.render_with_parallelism(document, 1)
    }

    pub fn render_with_parallelism(
        &self,
        document: &TypesetDocument,
        parallelism: usize,
    ) -> PdfDocument {
        self.render_with_partition_plan(
            document,
            parallelism,
            1,
            &DocumentPartitionPlan::default(),
            None,
        )
        .document
    }

    pub fn render_with_partition_plan(
        &self,
        document: &TypesetDocument,
        parallelism: usize,
        pass_number: u32,
        partition_plan: &DocumentPartitionPlan,
        pre_rendered_page_payloads: Option<&BTreeMap<usize, PageRenderPayload>>,
    ) -> RenderedPdfDocument {
        let page_count = document.pages.len();
        let total_lines = document.pages.iter().map(|page| page.lines.len()).sum();
        let link_style = &document.navigation.default_link_style;
        let mut pdf = Vec::<u8>::new();
        let mut offsets = BTreeMap::<usize, usize>::new();
        let page_object_start = 3usize;
        let content_object_start = page_object_start + page_count;
        let annotation_object_start = content_object_start + page_count;
        let mut image_objects = self.images.clone();
        let mut form_xobjects = self.form_xobjects.clone();
        let page_partition_ids = page_partition_ids_for_plan(document, partition_plan);
        let rendering_fonts = fonts_with_math_font(&self.fonts);
        let math_font_number = math_font_f_number(&rendering_fonts);
        let RenderedPagePayloads {
            page_payloads,
            encoding_errors,
        } = render_page_payloads(
            &document.pages,
            &self.page_images,
            &image_objects,
            &self.page_form_xobjects,
            &form_xobjects,
            link_style,
            &page_partition_ids,
            parallelism,
            pass_number,
            pre_rendered_page_payloads,
            math_font_number,
        );
        let mut page_annotations = page_payloads
            .iter()
            .map(|payload| payload.annotations.clone())
            .collect::<Vec<_>>();
        let next_object_after_annotations =
            assign_annotation_object_ids(&mut page_annotations, annotation_object_start);
        let opacity_graphics_state_object_ids =
            assign_opacity_graphics_state_object_ids(&page_payloads, next_object_after_annotations);
        let next_object_after_opacity_graphics_states =
            next_object_after_annotations + opacity_graphics_state_object_ids.len();
        let named_destination_object_id = (!document.named_destinations.is_empty())
            .then_some(next_object_after_opacity_graphics_states);
        let outline_root_object_id = (!document.outlines.is_empty()).then_some(
            next_object_after_opacity_graphics_states
                + usize::from(named_destination_object_id.is_some()),
        );
        let outline_item_object_start = next_object_after_opacity_graphics_states
            + usize::from(named_destination_object_id.is_some())
            + usize::from(outline_root_object_id.is_some());
        let outline_build =
            outline_root_object_id.map_or_else(OutlineBuildResult::default, |root_object_id| {
                build_outline_objects(
                    &document.outlines,
                    page_object_start,
                    root_object_id,
                    outline_item_object_start,
                )
            });
        let font_object_start = outline_item_object_start + outline_build.objects.len();
        let font_objects = build_font_objects(&rendering_fonts, font_object_start);
        let font_object_count = font_objects
            .iter()
            .map(|font_object| font_object.objects.len())
            .sum::<usize>();
        let image_object_start = font_object_start + font_object_count;
        assign_image_object_ids(&mut image_objects, image_object_start);
        let form_object_start = image_object_start + image_objects.len();
        assign_form_object_ids(&mut form_xobjects, form_object_start);
        let info_object_id =
            build_info_dictionary(document).map(|_| form_object_start + form_xobjects.len());
        let page_font_resources = page_font_resources(&font_objects);
        let catalog_named_destinations = named_destination_object_id
            .map(|object_id| format!(" /Names << /Dests {object_id} 0 R >>"))
            .unwrap_or_default();
        let catalog_outlines = outline_root_object_id
            .map(|object_id| format!(" /Outlines {object_id} 0 R"))
            .unwrap_or_default();

        pdf.extend_from_slice(b"%PDF-1.4\n");
        append_object(
            &mut pdf,
            &mut offsets,
            1,
            &format!(
                "1 0 obj\n<< /Type /Catalog /Pages 2 0 R{catalog_named_destinations}{catalog_outlines} >>\nendobj\n"
            ),
        );
        append_object(
            &mut pdf,
            &mut offsets,
            2,
            &format!(
                "2 0 obj\n<< /Type /Pages /Kids [{}] /Count {} >>\nendobj\n",
                page_kids(page_count, page_object_start),
                page_count
            ),
        );

        for (page_index, page) in document.pages.iter().enumerate() {
            let page_object_id = page_object_start + page_index;
            let content_object_id = content_object_start + page_index;
            let page_annots = page_annotations[page_index]
                .iter()
                .map(|annotation| format!("{} 0 R", annotation.object_id))
                .collect::<Vec<_>>();
            let page_images = self
                .page_images
                .get(page_index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let page_forms = self
                .page_form_xobjects
                .get(page_index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let annots_entry = if page_annots.is_empty() {
                String::new()
            } else {
                format!(" /Annots [{}]", page_annots.join(" "))
            };
            let xobject_resources =
                page_xobject_resources(page_images, &image_objects, page_forms, &form_xobjects);
            let opacity_resources = page_ext_gstate_resources(
                &page_payloads[page_index].opacity_graphics_states,
                &opacity_graphics_state_object_ids,
            );
            append_object(
                &mut pdf,
                &mut offsets,
                page_object_id,
                &format!(
                    "{page_object_id} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {content_object_id} 0 R /Resources << /Font << {} >>{}{} >>{annots_entry} >>\nendobj\n",
                    points_to_pdf_number(page.page_box.width),
                    points_to_pdf_number(page.page_box.height),
                    page_font_resources,
                    xobject_resources,
                    opacity_resources,
                ),
            );
        }

        for (page_index, payload) in page_payloads.iter().enumerate() {
            let content_object_id = content_object_start + page_index;
            append_object(
                &mut pdf,
                &mut offsets,
                content_object_id,
                &format!(
                    "{content_object_id} 0 obj\n<< /Length {} >>\nstream\n{}endstream\nendobj\n",
                    payload.stream.len(),
                    payload.stream
                ),
            );
        }

        for annotations in &page_annotations {
            for annotation in annotations {
                let (border, color) = if link_style.color_links {
                    ("[0 0 0]", "")
                } else {
                    ("[0 0 1]", " /C [0 0 1]")
                };
                let action = match &annotation.target {
                    PdfLinkTarget::Uri(url) => format!(
                        "/A << /Type /Action /S /URI /URI ({}) >>",
                        encode_pdf_text(url).encoded
                    ),
                    PdfLinkTarget::InternalDestination(name) => format!(
                        "/A << /Type /Action /S /GoTo /D ({}) >>",
                        encode_pdf_text(name).encoded
                    ),
                };
                append_object(
                    &mut pdf,
                    &mut offsets,
                    annotation.object_id,
                    &format!(
                        "{} 0 obj\n<< /Type /Annot /Subtype /Link /Rect [{} {} {} {}] /Border {}{} {} >>\nendobj\n",
                        annotation.object_id,
                        points_to_pdf_number(annotation.x_start),
                        points_to_pdf_number(annotation.y_bottom),
                        points_to_pdf_number(annotation.x_end),
                        points_to_pdf_number(annotation.y_top),
                        border,
                        color,
                        action,
                    ),
                );
            }
        }

        append_opacity_graphics_state_objects(
            &mut pdf,
            &mut offsets,
            &opacity_graphics_state_object_ids,
        );

        for image in &image_objects {
            append_image_xobject(&mut pdf, &mut offsets, image);
        }
        for form_xobject in &form_xobjects {
            append_form_xobject(&mut pdf, &mut offsets, form_xobject);
        }

        if let Some(object_id) = named_destination_object_id {
            append_object(
                &mut pdf,
                &mut offsets,
                object_id,
                &build_named_destination_object(document, page_object_start, object_id),
            );
        }

        if let Some(root_object_id) = outline_root_object_id {
            let first_object_id = outline_build.root_child_object_ids.first().copied();
            let last_object_id = outline_build.root_child_object_ids.last().copied();
            append_object(
                &mut pdf,
                &mut offsets,
                root_object_id,
                &format!(
                    "{root_object_id} 0 obj\n<< /Type /Outlines{}{} /Count {} >>\nendobj\n",
                    first_object_id
                        .map(|object_id| format!(" /First {object_id} 0 R"))
                        .unwrap_or_default(),
                    last_object_id
                        .map(|object_id| format!(" /Last {object_id} 0 R"))
                        .unwrap_or_default(),
                    outline_build.objects.len(),
                ),
            );

            for outline_object in &outline_build.objects {
                append_object(
                    &mut pdf,
                    &mut offsets,
                    outline_object.object_id,
                    &format!(
                        "{} 0 obj\n{}\nendobj\n",
                        outline_object.object_id, outline_object.body
                    ),
                );
            }
        }

        for font_object in &font_objects {
            for (index, object) in font_object.objects.iter().enumerate() {
                let object_id = font_object.dictionary_object_id + index;
                append_object_bytes(&mut pdf, &mut offsets, object_id, object);
            }
        }

        if let (Some(object_id), Some(info_dictionary)) =
            (info_object_id, build_info_dictionary(document))
        {
            append_object(
                &mut pdf,
                &mut offsets,
                object_id,
                &format!("{object_id} 0 obj\n{info_dictionary}\nendobj\n"),
            );
        }

        let xref_offset = pdf.len();
        let max_object_id = offsets.keys().copied().max().unwrap_or(0);
        debug_assert_eq!(
            offsets.len(),
            max_object_id,
            "PDF object ids must be contiguous starting at 1"
        );
        pdf.extend_from_slice(format!("xref\n0 {}\n", max_object_id + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for object_id in 1..=max_object_id {
            let offset = offsets
                .get(&object_id)
                .copied()
                .unwrap_or_else(|| panic!("missing xref entry for object {object_id}"));
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R{} >>\nstartxref\n{}\n%%EOF\n",
                max_object_id + 1,
                info_object_id
                    .map(|object_id| format!(" /Info {object_id} 0 R"))
                    .unwrap_or_default(),
                xref_offset
            )
            .as_bytes(),
        );

        RenderedPdfDocument {
            document: PdfDocument {
                bytes: pdf,
                page_count,
                total_lines,
                encoding_errors,
            },
            page_payloads,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageRenderPayload {
    pub page_index: usize,
    pub annotations: Vec<PdfLinkAnnotation>,
    pub opacity_graphics_states: BTreeSet<OpacityGraphicsStateKey>,
    pub stream_hash: u64,
    pub stream: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PartitionRenderPayload {
    partition_id: String,
    page_payloads: Vec<RenderedPagePayload>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PageRenderWorkload {
    partition_id: String,
    page_indices: Vec<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RenderedPagePayload {
    page_payload: PageRenderPayload,
    unencodable_chars: Vec<char>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct RenderedPagePayloads {
    page_payloads: Vec<PageRenderPayload>,
    encoding_errors: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PdfRenderStringResult {
    rendered: String,
    unencodable_chars: Vec<char>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PdfTextEncodeResult {
    encoded: String,
    unencodable_chars: Vec<char>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OpacityGraphicsStateKey {
    stroke_opacity_bits: u64,
    fill_opacity_bits: u64,
}

impl PageRenderPayload {
    pub fn new(
        page_index: usize,
        annotations: Vec<PdfLinkAnnotation>,
        opacity_graphics_states: BTreeSet<OpacityGraphicsStateKey>,
        stream: String,
    ) -> Self {
        let stream_hash =
            hash_page_payload_content(&stream, &annotations, &opacity_graphics_states);
        Self {
            page_index,
            annotations,
            opacity_graphics_states,
            stream_hash,
            stream,
        }
    }

    pub fn try_from_cached(
        page_index: usize,
        stream_hash: u64,
        stream: String,
        annotations: Vec<PdfLinkAnnotation>,
        opacity_graphics_states: BTreeSet<OpacityGraphicsStateKey>,
    ) -> Option<Self> {
        let computed_hash =
            hash_page_payload_content(&stream, &annotations, &opacity_graphics_states);
        (computed_hash == stream_hash).then_some(Self {
            page_index,
            annotations,
            opacity_graphics_states,
            stream_hash,
            stream,
        })
    }

    fn has_valid_stream_hash(&self) -> bool {
        self.stream_hash
            == hash_page_payload_content(
                &self.stream,
                &self.annotations,
                &self.opacity_graphics_states,
            )
    }
}

impl OpacityGraphicsStateKey {
    pub fn new(stroke_opacity: f64, fill_opacity: f64) -> Option<Self> {
        if !stroke_opacity.is_finite() || !fill_opacity.is_finite() {
            return None;
        }
        if stroke_opacity == 1.0 && fill_opacity == 1.0 {
            return None;
        }

        Some(Self {
            stroke_opacity_bits: stroke_opacity.to_bits(),
            fill_opacity_bits: fill_opacity.to_bits(),
        })
    }

    fn stroke_opacity(self) -> f64 {
        f64::from_bits(self.stroke_opacity_bits)
    }

    fn fill_opacity(self) -> f64 {
        f64::from_bits(self.fill_opacity_bits)
    }
}

fn hash_page_payload_content(
    stream: &str,
    annotations: &[PdfLinkAnnotation],
    opacity_graphics_states: &BTreeSet<OpacityGraphicsStateKey>,
) -> u64 {
    let mut hasher = DefaultHasher::new();
    stream.hash(&mut hasher);
    annotations.hash(&mut hasher);
    opacity_graphics_states.hash(&mut hasher);
    hasher.finish()
}

fn opacity_graphics_state_name(key: OpacityGraphicsStateKey) -> String {
    format!(
        "Gs{:016X}_{:016X}",
        key.stroke_opacity_bits, key.fill_opacity_bits
    )
}

fn default_font_resources() -> Vec<FontResource> {
    vec![FontResource::BuiltinType1 {
        base_font: "Helvetica".to_string(),
    }]
}

/// Returns a rendering-ready copy of `fonts` with the built-in Symbol font
/// appended (if absent). The Symbol font is always available so that math-mode
/// Unicode glyphs never have to fall through the WinAnsi text path.
fn fonts_with_math_font(fonts: &[FontResource]) -> Vec<FontResource> {
    let mut rendering_fonts = fonts.to_vec();
    if !rendering_fonts
        .iter()
        .any(|font| matches!(font, FontResource::SymbolBuiltin))
    {
        rendering_fonts.push(FontResource::SymbolBuiltin);
    }
    rendering_fonts
}

/// Returns the PDF `/Fn` number (1-based) that points at the Symbol font in
/// the rendering-ready fonts array produced by [`fonts_with_math_font`].
fn math_font_f_number(rendering_fonts: &[FontResource]) -> usize {
    rendering_fonts
        .iter()
        .position(|font| matches!(font, FontResource::SymbolBuiltin))
        .map(|index| index + 1)
        .expect("rendering fonts always contain the Symbol font")
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
                            "{dictionary_object_id} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /{} /Encoding /WinAnsiEncoding >>\nendobj\n",
                            base_font
                        )
                        .into_bytes(),
                    ],
                });
            }
            FontResource::SymbolBuiltin => {
                let dictionary_object_id = next_object_id;
                next_object_id += 1;

                font_objects.push(FontObjectSet {
                    dictionary_object_id,
                    objects: vec![format!(
                        "{dictionary_object_id} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>\nendobj\n"
                    )
                    .into_bytes()],
                });
            }
            FontResource::EmbeddedType1 {
                base_font,
                ascii_length,
                binary_length,
                trailer_length,
                font_program,
                first_char,
                last_char,
                widths,
                bbox,
                ascent,
                descent,
                italic_angle,
                stem_v,
                cap_height,
                flags,
            } => {
                let dictionary_object_id = next_object_id;
                let descriptor_object_id = next_object_id + 1;
                let font_file_object_id = next_object_id + 2;
                next_object_id += 3;

                let mut font_file_object = format!(
                    "{font_file_object_id} 0 obj\n<< /Length {} /Length1 {} /Length2 {} /Length3 {} >>\nstream\n",
                    font_program.len(),
                    ascii_length,
                    binary_length,
                    trailer_length
                )
                .into_bytes();
                font_file_object.extend_from_slice(font_program);
                font_file_object.extend_from_slice(b"\nendstream\nendobj\n");

                font_objects.push(FontObjectSet {
                    dictionary_object_id,
                    objects: vec![
                        format!(
                            "{dictionary_object_id} 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /{} /FirstChar {} /LastChar {} /Widths [{}] /FontDescriptor {} 0 R >>\nendobj\n",
                            base_font,
                            first_char,
                            last_char,
                            render_widths(widths),
                            descriptor_object_id
                        )
                        .into_bytes(),
                        format!(
                            "{descriptor_object_id} 0 obj\n<< /Type /FontDescriptor /FontName /{} /Flags {} /FontBBox [{} {} {} {}] /Ascent {} /Descent {} /ItalicAngle {} /StemV {} /CapHeight {} /FontFile {} 0 R >>\nendobj\n",
                            base_font,
                            flags,
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

#[allow(clippy::too_many_arguments)]
fn render_page_payloads(
    pages: &[TypesetPage],
    page_images: &[Vec<PlacedImage>],
    image_objects: &[PdfImageXObject],
    page_form_xobjects: &[Vec<PlacedFormXObject>],
    form_xobjects: &[PdfFormXObject],
    link_style: &LinkStyle,
    page_partition_ids: &[String],
    parallelism: usize,
    _pass_number: u32,
    pre_rendered_page_payloads: Option<&BTreeMap<usize, PageRenderPayload>>,
    math_font_number: usize,
) -> RenderedPagePayloads {
    let workloads = page_render_workloads(page_partition_ids, pages.len(), parallelism);
    let rendered_payloads = if workloads.len() <= 1 {
        workloads
            .into_iter()
            .flat_map(|workload| {
                workload
                    .page_indices
                    .into_iter()
                    .map(|page_index| {
                        render_page_payload_for_index(
                            pages,
                            page_images,
                            image_objects,
                            page_form_xobjects,
                            form_xobjects,
                            link_style,
                            pre_rendered_page_payloads,
                            page_index,
                            math_font_number,
                        )
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>()
    } else {
        let payloads = thread::scope(|scope| {
            let mut handles = Vec::new();
            for workload in workloads {
                handles.push(scope.spawn(move || {
                    let mut page_payloads = workload
                        .page_indices
                        .iter()
                        .copied()
                        .map(|page_index| {
                            render_page_payload_for_index(
                                pages,
                                page_images,
                                image_objects,
                                page_form_xobjects,
                                form_xobjects,
                                link_style,
                                pre_rendered_page_payloads,
                                page_index,
                                math_font_number,
                            )
                        })
                        .collect::<Vec<_>>();
                    page_payloads.sort_by_key(|payload| payload.page_payload.page_index);
                    PartitionRenderPayload {
                        partition_id: workload.partition_id,
                        page_payloads,
                    }
                }));
            }

            handles
                .into_iter()
                .rev()
                .map(|handle| handle.join().expect("page render worker should not panic"))
                .collect::<Vec<_>>()
        });
        let mut ordered_payloads = payloads;
        ordered_payloads.sort_by(|left, right| left.partition_id.cmp(&right.partition_id));
        ordered_payloads
            .into_iter()
            .flat_map(|payload| payload.page_payloads)
            .collect::<Vec<_>>()
    };

    let mut encoding_error_chars = BTreeSet::new();
    let page_payloads = rendered_payloads
        .into_iter()
        .map(|payload| {
            encoding_error_chars.extend(payload.unencodable_chars);
            payload.page_payload
        })
        .collect();

    RenderedPagePayloads {
        page_payloads,
        encoding_errors: encoding_error_chars
            .into_iter()
            .map(pdf_encoding_error)
            .collect(),
    }
}

#[allow(clippy::too_many_arguments)]
fn render_page_payload_for_index(
    pages: &[TypesetPage],
    page_images: &[Vec<PlacedImage>],
    image_objects: &[PdfImageXObject],
    page_form_xobjects: &[Vec<PlacedFormXObject>],
    form_xobjects: &[PdfFormXObject],
    link_style: &LinkStyle,
    pre_rendered_page_payloads: Option<&BTreeMap<usize, PageRenderPayload>>,
    page_index: usize,
    math_font_number: usize,
) -> RenderedPagePayload {
    let warning_chars = collect_page_warning_chars(&pages[page_index]);

    if !page_has_xobject_resources(page_images, page_form_xobjects, page_index) {
        if let Some(payload) = pre_rendered_page_payloads
            .and_then(|payloads| payloads.get(&page_index))
            .filter(|payload| payload.has_valid_stream_hash())
        {
            let mut cached_payload = payload.clone();
            cached_payload.page_index = page_index;
            return RenderedPagePayload {
                page_payload: cached_payload,
                unencodable_chars: warning_chars,
            };
        }
    }

    let render_result = render_page_stream(
        &pages[page_index],
        page_images
            .get(page_index)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        image_objects,
        page_form_xobjects
            .get(page_index)
            .map(Vec::as_slice)
            .unwrap_or(&[]),
        form_xobjects,
        link_style,
        math_font_number,
    );
    let page = &pages[page_index];
    RenderedPagePayload {
        page_payload: PageRenderPayload::new(
            page_index,
            page_link_annotations(page),
            collect_page_opacity_graphics_states(page),
            render_result.rendered,
        ),
        unencodable_chars: warning_chars,
    }
}

fn page_has_xobject_resources(
    page_images: &[Vec<PlacedImage>],
    page_form_xobjects: &[Vec<PlacedFormXObject>],
    page_index: usize,
) -> bool {
    page_images
        .get(page_index)
        .is_some_and(|images| !images.is_empty())
        || page_form_xobjects
            .get(page_index)
            .is_some_and(|forms| !forms.is_empty())
}

pub fn page_partition_ids_for_plan(
    document: &TypesetDocument,
    partition_plan: &DocumentPartitionPlan,
) -> Vec<String> {
    let mut page_partition_ids =
        vec![partition_plan.fallback_partition_id.clone(); document.pages.len()];
    if document.pages.is_empty() {
        return page_partition_ids;
    }

    let mut markers = Vec::new();
    let mut outline_cursor = 0usize;
    for work_unit in &partition_plan.work_units {
        if let Some(offset) = document.outlines[outline_cursor..]
            .iter()
            .position(|outline| {
                outline.level == work_unit.locator.level && outline.title == work_unit.title
            })
        {
            let outline_index = outline_cursor + offset;
            markers.push((
                document.outlines[outline_index].page_index,
                work_unit.partition_id.clone(),
            ));
            outline_cursor = outline_index + 1;
        }
    }

    if markers.is_empty() {
        return page_partition_ids;
    }

    for (marker_index, (start_page, partition_id)) in markers.iter().enumerate() {
        let start = (*start_page).min(page_partition_ids.len());
        let end = markers
            .get(marker_index + 1)
            .map(|(next_page, _)| (*next_page).min(page_partition_ids.len()))
            .unwrap_or(page_partition_ids.len());
        for page_partition_id in &mut page_partition_ids[start..end] {
            *page_partition_id = partition_id.clone();
        }
    }

    page_partition_ids
}

fn page_render_workloads(
    page_partition_ids: &[String],
    page_count: usize,
    parallelism: usize,
) -> Vec<PageRenderWorkload> {
    if page_count == 0 {
        return Vec::new();
    }

    let mut workloads = if page_partition_ids.len() == page_count {
        let mut workloads = Vec::new();
        let mut start = 0usize;
        while start < page_count {
            let partition_id = page_partition_ids[start].clone();
            let mut end = start + 1;
            while end < page_count && page_partition_ids[end] == partition_id {
                end += 1;
            }
            workloads.push(PageRenderWorkload {
                partition_id,
                page_indices: (start..end).collect(),
            });
            start = end;
        }
        workloads
    } else {
        vec![PageRenderWorkload {
            partition_id: "document:0000:root".to_string(),
            page_indices: (0..page_count).collect(),
        }]
    };

    if workloads.len() == 1 && parallelism > 1 && page_count > 1 {
        let concurrency = parallelism.max(1).min(page_count);
        let chunk_size = page_count.div_ceil(concurrency);
        let base_partition_id = workloads[0].partition_id.clone();
        workloads = (0..page_count)
            .step_by(chunk_size)
            .enumerate()
            .map(|(chunk_index, start)| PageRenderWorkload {
                partition_id: format!("{base_partition_id}:chunk-{chunk_index:04}"),
                page_indices: (start..(start + chunk_size).min(page_count)).collect(),
            })
            .collect();
    }

    workloads
}

fn render_page_stream(
    page: &TypesetPage,
    placed_images: &[PlacedImage],
    image_objects: &[PdfImageXObject],
    placed_form_xobjects: &[PlacedFormXObject],
    form_xobjects: &[PdfFormXObject],
    link_style: &LinkStyle,
    math_font_number: usize,
) -> PdfRenderStringResult {
    let mut stream = String::new();
    let mut warning_chars = BTreeSet::new();

    let rendered_lines = render_text_lines(&page.lines, link_style, math_font_number);
    warning_chars.extend(rendered_lines.unencodable_chars);
    stream.push_str(&rendered_lines.rendered);
    for placement in &page.float_placements {
        let lines = resolve_float_lines(placement);
        let rendered_lines = render_text_lines(&lines, link_style, math_font_number);
        warning_chars.extend(rendered_lines.unencodable_chars);
        stream.push_str(&rendered_lines.rendered);
    }

    let mut image_index = 0usize;
    let mut form_index = 0usize;
    for image in &page.images {
        collect_scene_warning_chars(&image.scene, &mut warning_chars);
        match single_scene_node(&image.scene) {
            Some(GraphicNode::External(_)) => {
                if let Some(placement) = placed_images.get(image_index) {
                    if image_objects.get(placement.xobject_index).is_some() {
                        stream.push_str(&render_image_placement(placement));
                    }
                }
                image_index += 1;
            }
            Some(GraphicNode::Pdf(_)) => {
                if let Some(placement) = placed_form_xobjects.get(form_index) {
                    if let Some(form_xobject) = form_xobjects.get(placement.xobject_index) {
                        stream.push_str(&render_form_xobject_placement(placement, form_xobject));
                    }
                }
                form_index += 1;
            }
            _ => stream.push_str(&render_graphics_scene_placement(
                &image.scene,
                image.x,
                image.y,
            )),
        }
    }

    PdfRenderStringResult {
        rendered: stream,
        unencodable_chars: warning_chars.into_iter().collect(),
    }
}

fn collect_page_opacity_graphics_states(page: &TypesetPage) -> BTreeSet<OpacityGraphicsStateKey> {
    let mut opacity_graphics_states = BTreeSet::new();
    for image in &page.images {
        collect_scene_opacity_graphics_states(&image.scene, &mut opacity_graphics_states);
    }
    opacity_graphics_states
}

fn collect_page_warning_chars(page: &TypesetPage) -> Vec<char> {
    let mut warning_chars = BTreeSet::new();
    collect_text_line_warning_chars(&page.lines, &mut warning_chars);
    for placement in &page.float_placements {
        collect_text_line_warning_chars(&resolve_float_lines(placement), &mut warning_chars);
    }
    for image in &page.images {
        collect_scene_warning_chars(&image.scene, &mut warning_chars);
    }
    warning_chars.into_iter().collect()
}

fn collect_scene_opacity_graphics_states(
    scene: &GraphicsScene,
    opacity_graphics_states: &mut BTreeSet<OpacityGraphicsStateKey>,
) {
    for node in &scene.nodes {
        collect_graphic_node_opacity_graphics_states(node, opacity_graphics_states);
    }
}

fn collect_scene_warning_chars(scene: &GraphicsScene, warning_chars: &mut BTreeSet<char>) {
    for node in &scene.nodes {
        collect_graphic_node_warning_chars(node, warning_chars);
    }
}

fn collect_graphic_node_opacity_graphics_states(
    node: &GraphicNode,
    opacity_graphics_states: &mut BTreeSet<OpacityGraphicsStateKey>,
) {
    match node {
        GraphicNode::External(_) | GraphicNode::Pdf(_) | GraphicNode::Text(_) => {}
        GraphicNode::Group(group) => {
            for child in &group.children {
                collect_graphic_node_opacity_graphics_states(child, opacity_graphics_states);
            }
        }
        GraphicNode::Vector(primitive) => {
            if let Some(key) =
                OpacityGraphicsStateKey::new(primitive.opacity, primitive.fill_opacity)
            {
                opacity_graphics_states.insert(key);
            }
        }
    }
}

fn collect_graphic_node_warning_chars(node: &GraphicNode, warning_chars: &mut BTreeSet<char>) {
    match node {
        GraphicNode::External(_) | GraphicNode::Pdf(_) | GraphicNode::Vector(_) => {}
        GraphicNode::Group(group) => {
            for child in &group.children {
                collect_graphic_node_warning_chars(child, warning_chars);
            }
        }
        GraphicNode::Text(text) => {
            collect_unencodable_chars(&text.content, warning_chars);
        }
    }
}

fn collect_text_line_warning_chars(lines: &[TextLine], warning_chars: &mut BTreeSet<char>) {
    for line in lines {
        collect_unencodable_chars(&strip_math_script_markers(&line.text), warning_chars);
    }
}

fn render_text_lines(
    lines: &[TextLine],
    link_style: &LinkStyle,
    math_font_number: usize,
) -> PdfRenderStringResult {
    let Some(first_line) = lines.first() else {
        return PdfRenderStringResult {
            rendered: String::new(),
            unencodable_chars: Vec::new(),
        };
    };

    let mut stream = format!(
        "BT\n/F{} {} Tf\n",
        first_line.font_index + 1,
        points_to_pdf_number(first_line.font_size)
    );
    let mut warning_chars = BTreeSet::new();
    let mut current_font = first_line.font_index;
    let mut current_font_size = first_line.font_size;
    let first_x = LEFT_MARGIN_PT as i64 * SCALED_POINTS_PER_POINT + first_line.x.0;
    stream.push_str(&format!(
        "{} {} Td\n",
        points_to_pdf_number(DimensionValue(first_x)),
        points_to_pdf_number(first_line.y)
    ));
    render_text_line(
        &mut stream,
        first_line,
        link_style,
        &mut warning_chars,
        math_font_number,
    );

    let mut previous_x = first_line.x;
    let mut previous_y = first_line.y;
    for line in &lines[1..] {
        let dx = line.x.0 - previous_x.0;
        stream.push_str(&format!(
            "{} {} Td\n",
            points_to_pdf_number(DimensionValue(dx)),
            points_to_pdf_number(line.y - previous_y)
        ));
        if line.font_index != current_font || line.font_size != current_font_size {
            stream.push_str(&format!(
                "/F{} {} Tf\n",
                line.font_index + 1,
                points_to_pdf_number(line.font_size)
            ));
            current_font = line.font_index;
            current_font_size = line.font_size;
        }
        render_text_line(
            &mut stream,
            line,
            link_style,
            &mut warning_chars,
            math_font_number,
        );
        previous_x = line.x;
        previous_y = line.y;
    }
    stream.push_str("ET\n");
    PdfRenderStringResult {
        rendered: stream,
        unencodable_chars: warning_chars.into_iter().collect(),
    }
}

fn render_text_line(
    stream: &mut String,
    line: &TextLine,
    link_style: &LinkStyle,
    warning_chars: &mut BTreeSet<char>,
    math_font_number: usize,
) {
    if contains_script_markers(&line.text) && line.links.is_empty() {
        render_text_line_with_scripts(stream, line, warning_chars, math_font_number);
        return;
    }

    let primary_font_number = usize::from(line.font_index) + 1;
    let font_size = line.font_size;

    let Some(link_color) = active_link_color(link_style) else {
        emit_text_with_font_runs(
            stream,
            &strip_math_script_markers(&line.text),
            primary_font_number,
            math_font_number,
            font_size,
            warning_chars,
        );
        return;
    };

    let mut links = line
        .links
        .iter()
        .filter(|link| !link.url.is_empty() && link.start_char < link.end_char)
        .collect::<Vec<_>>();
    if links.is_empty() {
        emit_text_with_font_runs(
            stream,
            &strip_math_script_markers(&line.text),
            primary_font_number,
            math_font_number,
            font_size,
            warning_chars,
        );
        return;
    }

    links.sort_by_key(|link| (link.start_char, link.end_char));
    let rendered_text = strip_math_script_markers(&line.text);
    let boundaries = char_boundaries(&rendered_text);
    let char_count = boundaries.len().saturating_sub(1);
    let mut cursor = 0usize;
    for link in links {
        let start = link.start_char.min(char_count);
        let end = link.end_char.min(char_count);
        if start >= end || start < cursor {
            continue;
        }
        if cursor < start {
            emit_text_with_font_runs(
                stream,
                char_slice(&rendered_text, &boundaries, cursor, start),
                primary_font_number,
                math_font_number,
                font_size,
                warning_chars,
            );
        }
        stream.push_str(&pdf_rgb_operator(link_color));
        emit_text_with_font_runs(
            stream,
            char_slice(&rendered_text, &boundaries, start, end),
            primary_font_number,
            math_font_number,
            font_size,
            warning_chars,
        );
        stream.push_str("0 0 0 rg\n");
        cursor = end;
    }

    if cursor < char_count {
        emit_text_with_font_runs(
            stream,
            char_slice(&rendered_text, &boundaries, cursor, char_count),
            primary_font_number,
            math_font_number,
            font_size,
            warning_chars,
        );
    }
}

fn render_text_line_with_scripts(
    stream: &mut String,
    line: &TextLine,
    warning_chars: &mut BTreeSet<char>,
    math_font_number: usize,
) {
    let base_font = usize::from(line.font_index) + 1;
    let base_font_size = line.font_size;
    let script_font_size = scaled_font_size(base_font_size, 7, 10);
    let rendered_text = strip_math_script_markers(&line.text);

    stream.push_str(&format!(
        "/Span <</ActualText {}>> BDC\n",
        encode_pdf_actual_text(&rendered_text)
    ));

    for segment in parse_math_script_segments(&line.text) {
        if segment.text.is_empty() {
            continue;
        }

        match segment.kind {
            ScriptSegmentKind::Base => {
                emit_text_with_font_runs(
                    stream,
                    &segment.text,
                    base_font,
                    math_font_number,
                    base_font_size,
                    warning_chars,
                );
            }
            ScriptSegmentKind::Superscript => {
                stream.push_str(&format!("{SUPERSCRIPT_RISE_PT} Ts\n"));
                stream.push_str(&format!(
                    "/F{base_font} {} Tf\n",
                    points_to_pdf_number(script_font_size)
                ));
                emit_text_with_font_runs(
                    stream,
                    &segment.text,
                    base_font,
                    math_font_number,
                    script_font_size,
                    warning_chars,
                );
                stream.push_str(&format!(
                    "/F{base_font} {} Tf\n",
                    points_to_pdf_number(base_font_size)
                ));
                stream.push_str("0 Ts\n");
            }
            ScriptSegmentKind::FootnoteSuperscript => {
                stream.push_str(&format!("{FOOTNOTE_SUPERSCRIPT_RISE_PT} Ts\n"));
                stream.push_str(&format!(
                    "/F{base_font} {} Tf\n",
                    points_to_pdf_number(script_font_size)
                ));
                emit_text_with_font_runs(
                    stream,
                    &segment.text,
                    base_font,
                    math_font_number,
                    script_font_size,
                    warning_chars,
                );
                stream.push_str(&format!(
                    "/F{base_font} {} Tf\n",
                    points_to_pdf_number(base_font_size)
                ));
                stream.push_str("0 Ts\n");
            }
            ScriptSegmentKind::Subscript => {
                stream.push_str(&format!("{} Ts\n", -SUBSCRIPT_DROP_PT));
                stream.push_str(&format!(
                    "/F{base_font} {} Tf\n",
                    points_to_pdf_number(script_font_size)
                ));
                emit_text_with_font_runs(
                    stream,
                    &segment.text,
                    base_font,
                    math_font_number,
                    script_font_size,
                    warning_chars,
                );
                stream.push_str(&format!(
                    "/F{base_font} {} Tf\n",
                    points_to_pdf_number(base_font_size)
                ));
                stream.push_str("0 Ts\n");
            }
        }
    }

    stream.push_str("EMC\n");
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScriptSegmentKind {
    Base,
    Superscript,
    FootnoteSuperscript,
    Subscript,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScriptSegment {
    kind: ScriptSegmentKind,
    text: String,
}

fn parse_math_script_segments(text: &str) -> Vec<ScriptSegment> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut current_kind = ScriptSegmentKind::Base;

    for ch in text.chars() {
        let next_kind = match ch {
            SUPERSCRIPT_START_MARKER => Some(ScriptSegmentKind::Superscript),
            FOOTNOTE_MARKER_START => Some(ScriptSegmentKind::FootnoteSuperscript),
            SUPERSCRIPT_END_MARKER | SUBSCRIPT_END_MARKER => Some(ScriptSegmentKind::Base),
            FOOTNOTE_MARKER_END => Some(ScriptSegmentKind::Base),
            SUBSCRIPT_START_MARKER => Some(ScriptSegmentKind::Subscript),
            _ => None,
        };

        if let Some(kind) = next_kind {
            if !current.is_empty() {
                segments.push(ScriptSegment {
                    kind: current_kind,
                    text: std::mem::take(&mut current),
                });
            }
            current_kind = kind;
            continue;
        }

        current.push(ch);
    }

    if !current.is_empty() {
        segments.push(ScriptSegment {
            kind: current_kind,
            text: current,
        });
    }

    segments
}

fn contains_script_markers(text: &str) -> bool {
    text.contains(FOOTNOTE_MARKER_START)
        || text.contains(FOOTNOTE_MARKER_END)
        || text.contains(SUPERSCRIPT_START_MARKER)
        || text.contains(SUPERSCRIPT_END_MARKER)
        || text.contains(SUBSCRIPT_START_MARKER)
        || text.contains(SUBSCRIPT_END_MARKER)
}

fn strip_math_script_markers(text: &str) -> String {
    text.chars()
        .filter(|ch| {
            !matches!(
                *ch,
                SUPERSCRIPT_START_MARKER
                    | SUPERSCRIPT_END_MARKER
                    | FOOTNOTE_MARKER_START
                    | FOOTNOTE_MARKER_END
                    | SUBSCRIPT_START_MARKER
                    | SUBSCRIPT_END_MARKER
            )
        })
        .collect()
}

fn scaled_font_size(size: DimensionValue, numerator: i64, denominator: i64) -> DimensionValue {
    DimensionValue((size.0 * numerator + denominator / 2) / denominator)
}

fn resolve_float_lines(placement: &FloatPlacement) -> Vec<TextLine> {
    placement
        .content
        .lines
        .iter()
        .map(|line| TextLine {
            text: line.text.clone(),
            x: line.x,
            y: placement.y_position - line.y,
            links: line.links.clone(),
            font_index: line.font_index,
            font_size: line.font_size,
            source_span: line.source_span,
        })
        .collect()
}

fn resolve_named_color(name: &str) -> Option<(f64, f64, f64)> {
    match name.to_ascii_lowercase().as_str() {
        "red" => Some((1.0, 0.0, 0.0)),
        "blue" => Some((0.0, 0.0, 1.0)),
        "green" => Some((0.0, 1.0, 0.0)),
        "cyan" => Some((0.0, 1.0, 1.0)),
        "magenta" => Some((1.0, 0.0, 1.0)),
        "yellow" => Some((1.0, 1.0, 0.0)),
        "black" => Some((0.0, 0.0, 0.0)),
        "white" => Some((1.0, 1.0, 1.0)),
        "darkgray" | "darkgrey" => Some((0.25, 0.25, 0.25)),
        "gray" | "grey" => Some((0.5, 0.5, 0.5)),
        "lightgray" | "lightgrey" => Some((0.75, 0.75, 0.75)),
        "brown" => Some((0.75, 0.5, 0.25)),
        "olive" => Some((0.5, 0.5, 0.0)),
        "orange" => Some((1.0, 0.5, 0.0)),
        "pink" => Some((1.0, 0.75, 0.75)),
        "purple" => Some((0.75, 0.0, 0.25)),
        "teal" => Some((0.0, 0.5, 0.5)),
        "violet" => Some((0.5, 0.0, 0.5)),
        _ => None,
    }
}

fn active_link_color(link_style: &LinkStyle) -> Option<(f64, f64, f64)> {
    link_style.color_links.then(|| {
        link_style
            .link_color
            .as_deref()
            .and_then(resolve_named_color)
            .unwrap_or((0.0, 0.0, 1.0))
    })
}

fn pdf_rgb_operator((red, green, blue): (f64, f64, f64)) -> String {
    format!("{red} {green} {blue} rg\n")
}

fn pdf_stroke_rgb_operator((red, green, blue): (f64, f64, f64)) -> String {
    format!("{red} {green} {blue} RG\n")
}

fn char_boundaries(text: &str) -> Vec<usize> {
    let mut boundaries = text
        .char_indices()
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    boundaries.push(text.len());
    boundaries
}

fn char_slice<'a>(text: &'a str, boundaries: &[usize], start: usize, end: usize) -> &'a str {
    &text[boundaries[start]..boundaries[end]]
}

fn render_image_placement(image: &PlacedImage) -> String {
    format!(
        "q {} 0 0 {} {} {} cm /Im{} Do Q\n",
        points_to_pdf_number(image.display_width),
        points_to_pdf_number(image.display_height),
        points_to_pdf_number(image.x),
        points_to_pdf_number(image.y),
        image.xobject_index + 1,
    )
}

fn single_scene_node(scene: &GraphicsScene) -> Option<&GraphicNode> {
    (scene.nodes.len() == 1).then(|| &scene.nodes[0])
}

fn render_graphics_scene_placement(
    scene: &GraphicsScene,
    x: DimensionValue,
    y: DimensionValue,
) -> String {
    let mut body = String::new();
    for node in &scene.nodes {
        body.push_str(&render_graphic_node(node));
    }

    if body.is_empty() {
        return String::new();
    }

    format!(
        "q 1 0 0 1 {} {} cm\n{}Q\n",
        pdf_real(dimension_to_pdf_number(x)),
        pdf_real(dimension_to_pdf_number(y)),
        body
    )
}

fn render_graphic_node(node: &GraphicNode) -> String {
    match node {
        GraphicNode::External(_) | GraphicNode::Pdf(_) => String::new(),
        GraphicNode::Group(group) => render_graphic_group(group),
        GraphicNode::Vector(primitive) => render_vector_primitive(primitive),
        GraphicNode::Text(text) => render_graphic_text(text),
    }
}

fn render_graphic_group(group: &GraphicGroup) -> String {
    let mut body = String::new();
    body.push_str("q\n");

    let radians = group.transform.rotate.to_radians();
    let cos_theta = radians.cos() * group.transform.scale;
    let sin_theta = radians.sin() * group.transform.scale;
    body.push_str(&format!(
        "{} {} {} {} {} {} cm\n",
        pdf_real(cos_theta),
        pdf_real(sin_theta),
        pdf_real(-sin_theta),
        pdf_real(cos_theta),
        pdf_real(group.transform.x_shift),
        pdf_real(group.transform.y_shift),
    ));

    if let Some(clip_path) = &group.clip_path {
        body.push_str(&render_path_segments(clip_path));
        body.push_str("W n\n");
    }

    for child in &group.children {
        body.push_str(&render_graphic_node(child));
    }

    body.push_str("Q\n");
    body
}

fn render_vector_primitive(primitive: &crate::graphics::api::VectorPrimitive) -> String {
    if primitive.path.is_empty() {
        return String::new();
    }

    let mut stream = String::new();
    stream.push_str(&format!("{} w\n", pdf_real(primitive.line_width)));
    match primitive.dash_pattern {
        DashPattern::Solid => {}
        DashPattern::Dashed => stream.push_str("[3 3] 0 d\n"),
        DashPattern::Dotted => stream.push_str("[1 2] 0 d\n"),
        DashPattern::DenselyDashed => stream.push_str("[3 2] 0 d\n"),
        DashPattern::DenselyDotted => stream.push_str("[1 1] 0 d\n"),
        DashPattern::LooselyDashed => stream.push_str("[3 6] 0 d\n"),
        DashPattern::LooselyDotted => stream.push_str("[1 4] 0 d\n"),
        DashPattern::DashDot => stream.push_str("[3 2 1 2] 0 d\n"),
        DashPattern::DashDotDot => stream.push_str("[3 2 1 2 1 2] 0 d\n"),
    }
    match primitive.line_cap {
        LineCap::Butt => {}
        LineCap::Round => stream.push_str("1 J\n"),
        LineCap::Rect => stream.push_str("2 J\n"),
    }
    match primitive.line_join {
        LineJoin::Miter => {}
        LineJoin::Round => stream.push_str("1 j\n"),
        LineJoin::Bevel => stream.push_str("2 j\n"),
    }
    if let Some(key) = OpacityGraphicsStateKey::new(primitive.opacity, primitive.fill_opacity) {
        stream.push_str(&format!("/{} gs\n", opacity_graphics_state_name(key)));
    }
    if let Some(stroke) = primitive.stroke {
        stream.push_str(&pdf_stroke_rgb_operator(color_components(stroke)));
    }
    if let Some(fill) = primitive.fill {
        stream.push_str(&pdf_rgb_operator(color_components(fill)));
    }

    stream.push_str(&render_path_segments(&primitive.path));

    stream.push_str(
        match (primitive.stroke.is_some(), primitive.fill.is_some()) {
            (true, true) => "B\n",
            (true, false) => "S\n",
            (false, true) => "f\n",
            (false, false) => "n\n",
        },
    );

    stream.push_str(&render_arrowheads(primitive));
    stream
}

fn render_path_segments(path: &[PathSegment]) -> String {
    let mut stream = String::new();
    for segment in path {
        match segment {
            PathSegment::MoveTo(point) => {
                stream.push_str(&format!("{} {} m\n", pdf_real(point.x), pdf_real(point.y)));
            }
            PathSegment::LineTo(point) => {
                stream.push_str(&format!("{} {} l\n", pdf_real(point.x), pdf_real(point.y)));
            }
            PathSegment::CurveTo {
                control1,
                control2,
                end,
            } => {
                stream.push_str(&format!(
                    "{} {} {} {} {} {} c\n",
                    pdf_real(control1.x),
                    pdf_real(control1.y),
                    pdf_real(control2.x),
                    pdf_real(control2.y),
                    pdf_real(end.x),
                    pdf_real(end.y),
                ));
            }
            PathSegment::ClosePath => stream.push_str("h\n"),
        }
    }
    stream
}

fn render_arrowheads(primitive: &crate::graphics::api::VectorPrimitive) -> String {
    if primitive.arrows == ArrowSpec::None {
        return String::new();
    }

    let Some(color) = primitive.stroke.or(primitive.fill).or(Some(Color {
        r: 0.0,
        g: 0.0,
        b: 0.0,
    })) else {
        return String::new();
    };

    let (start, end) = path_arrow_anchors(&primitive.path);
    let mut stream = String::new();

    if matches!(primitive.arrows, ArrowSpec::Backward | ArrowSpec::Both) {
        if let Some((tip, reference)) = start {
            stream.push_str(&render_arrowhead(tip, reference, color));
        }
    }
    if matches!(primitive.arrows, ArrowSpec::Forward | ArrowSpec::Both) {
        if let Some((tip, reference)) = end {
            stream.push_str(&render_arrowhead(tip, reference, color));
        }
    }

    stream
}

type ArrowAnchor = (Point, Point);

fn path_arrow_anchors(path: &[PathSegment]) -> (Option<ArrowAnchor>, Option<ArrowAnchor>) {
    let mut current = None;
    let mut subpath_start = None;
    let mut first_anchor = None;
    let mut last_anchor = None;

    for segment in path {
        match segment {
            PathSegment::MoveTo(point) => {
                current = Some(*point);
                subpath_start = Some(*point);
            }
            PathSegment::LineTo(point) => {
                if let Some(start) = current {
                    first_anchor.get_or_insert((start, *point));
                    last_anchor = Some((*point, start));
                }
                current = Some(*point);
            }
            PathSegment::CurveTo {
                control1,
                control2,
                end,
            } => {
                if let Some(start) = current {
                    first_anchor.get_or_insert((start, *control1));
                    last_anchor = Some((*end, *control2));
                }
                current = Some(*end);
            }
            PathSegment::ClosePath => {
                if let (Some(start), Some(end)) = (subpath_start, current) {
                    if end != start {
                        first_anchor.get_or_insert((start, end));
                        last_anchor = Some((start, end));
                    }
                    current = Some(start);
                }
            }
        }
    }

    (first_anchor, last_anchor)
}

fn render_arrowhead(tip: Point, reference: Point, color: Color) -> String {
    let dx = tip.x - reference.x;
    let dy = tip.y - reference.y;
    let length = (dx * dx + dy * dy).sqrt();
    if length <= f64::EPSILON {
        return String::new();
    }

    let ux = dx / length;
    let uy = dy / length;
    let arrow_length = 4.0;
    let half_width = 2.0;
    let base = Point {
        x: tip.x - (ux * arrow_length),
        y: tip.y - (uy * arrow_length),
    };
    let left = Point {
        x: base.x - (uy * half_width),
        y: base.y + (ux * half_width),
    };
    let right = Point {
        x: base.x + (uy * half_width),
        y: base.y - (ux * half_width),
    };

    format!(
        "{}{} {} m\n{} {} l\n{} {} l\nh\nf\n",
        pdf_rgb_operator(color_components(color)),
        pdf_real(tip.x),
        pdf_real(tip.y),
        pdf_real(left.x),
        pdf_real(left.y),
        pdf_real(right.x),
        pdf_real(right.y),
    )
}

fn render_graphic_text(text: &crate::graphics::api::GraphicText) -> String {
    format!(
        "BT\n/F1 12 Tf\n0 0 0 rg\n{} {} Td\n({}) Tj\nET\n",
        pdf_real(text.position.x),
        pdf_real(text.position.y),
        encode_pdf_text(&text.content).encoded
    )
}

fn color_components(color: Color) -> (f64, f64, f64) {
    (color.r, color.g, color.b)
}

fn render_form_xobject_placement(
    form: &PlacedFormXObject,
    form_xobject: &PdfFormXObject,
) -> String {
    let natural_width = form_xobject.media_box[2] - form_xobject.media_box[0];
    let natural_height = form_xobject.media_box[3] - form_xobject.media_box[1];
    if natural_width <= 0.0 || natural_height <= 0.0 {
        return String::new();
    }

    let scale_x = dimension_to_pdf_number(form.display_width) / natural_width;
    let scale_y = dimension_to_pdf_number(form.display_height) / natural_height;
    let translate_x = dimension_to_pdf_number(form.x) - form_xobject.media_box[0] * scale_x;
    let translate_y = dimension_to_pdf_number(form.y) - form_xobject.media_box[1] * scale_y;

    format!(
        "q {} 0 0 {} {} {} cm /Fm{} Do Q\n",
        pdf_real(scale_x),
        pdf_real(scale_y),
        pdf_real(translate_x),
        pdf_real(translate_y),
        form.xobject_index + 1,
    )
}

fn assign_image_object_ids(images: &mut [PdfImageXObject], start_object_id: usize) {
    for (index, image) in images.iter_mut().enumerate() {
        image.object_id = start_object_id + index;
    }
}

fn assign_form_object_ids(forms: &mut [PdfFormXObject], start_object_id: usize) {
    for (index, form) in forms.iter_mut().enumerate() {
        form.object_id = start_object_id + index;
    }
}

fn page_xobject_resources(
    page_images: &[PlacedImage],
    image_objects: &[PdfImageXObject],
    page_forms: &[PlacedFormXObject],
    form_xobjects: &[PdfFormXObject],
) -> String {
    let mut resources = page_images
        .iter()
        .filter_map(|placement| {
            image_objects
                .get(placement.xobject_index)
                .map(|image| format!("/Im{} {} 0 R", placement.xobject_index + 1, image.object_id))
        })
        .collect::<std::collections::BTreeSet<_>>();
    resources.extend(page_forms.iter().filter_map(|placement| {
        form_xobjects
            .get(placement.xobject_index)
            .map(|form| format!("/Fm{} {} 0 R", placement.xobject_index + 1, form.object_id))
    }));

    if resources.is_empty() {
        String::new()
    } else {
        format!(
            " /XObject << {} >>",
            resources.into_iter().collect::<Vec<_>>().join(" ")
        )
    }
}

fn assign_opacity_graphics_state_object_ids(
    page_payloads: &[PageRenderPayload],
    start_object_id: usize,
) -> BTreeMap<OpacityGraphicsStateKey, usize> {
    let mut object_ids = BTreeMap::new();
    let mut next_object_id = start_object_id;

    for key in page_payloads
        .iter()
        .flat_map(|payload| payload.opacity_graphics_states.iter().copied())
    {
        object_ids.entry(key).or_insert_with(|| {
            let object_id = next_object_id;
            next_object_id += 1;
            object_id
        });
    }

    object_ids
}

fn page_ext_gstate_resources(
    opacity_graphics_states: &BTreeSet<OpacityGraphicsStateKey>,
    object_ids: &BTreeMap<OpacityGraphicsStateKey, usize>,
) -> String {
    if opacity_graphics_states.is_empty() {
        return String::new();
    }

    let resources = opacity_graphics_states
        .iter()
        .filter_map(|key| {
            object_ids.get(key).map(|object_id| {
                format!("/{} {} 0 R", opacity_graphics_state_name(*key), object_id)
            })
        })
        .collect::<Vec<_>>();

    if resources.is_empty() {
        String::new()
    } else {
        format!(" /ExtGState << {} >>", resources.join(" "))
    }
}

fn append_opacity_graphics_state_objects(
    buffer: &mut Vec<u8>,
    offsets: &mut BTreeMap<usize, usize>,
    object_ids: &BTreeMap<OpacityGraphicsStateKey, usize>,
) {
    for (key, object_id) in object_ids {
        append_object(
            buffer,
            offsets,
            *object_id,
            &format!(
                "{object_id} 0 obj\n<< /Type /ExtGState /CA {} /ca {} >>\nendobj\n",
                pdf_real(key.stroke_opacity()),
                pdf_real(key.fill_opacity()),
            ),
        );
    }
}

fn append_image_xobject(
    buffer: &mut Vec<u8>,
    offsets: &mut BTreeMap<usize, usize>,
    image: &PdfImageXObject,
) {
    record_object_offset(offsets, image.object_id, buffer.len());
    buffer.extend_from_slice(
        format!(
            "{} 0 obj\n<< /Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /{} /BitsPerComponent {} /Filter /{}{} /Length {} >>\nstream\n",
            image.object_id,
            image.width,
            image.height,
            image_color_space_name(image.color_space),
            image.bits_per_component,
            image_filter_name(image.filter),
            image_decode_params(image),
            image.data.len(),
        )
        .as_bytes(),
    );
    buffer.extend_from_slice(&image.data);
    buffer.extend_from_slice(b"\nendstream\nendobj\n");
}

fn append_form_xobject(
    buffer: &mut Vec<u8>,
    offsets: &mut BTreeMap<usize, usize>,
    form_xobject: &PdfFormXObject,
) {
    record_object_offset(offsets, form_xobject.object_id, buffer.len());
    let resources_dict = form_xobject.resources_dict.as_deref().unwrap_or("<< >>");
    buffer.extend_from_slice(
        format!(
            "{} 0 obj\n<< /Type /XObject /Subtype /Form /FormType 1 /BBox [{} {} {} {}] /Resources {} /Length {} >>\nstream\n",
            form_xobject.object_id,
            pdf_real(form_xobject.media_box[0]),
            pdf_real(form_xobject.media_box[1]),
            pdf_real(form_xobject.media_box[2]),
            pdf_real(form_xobject.media_box[3]),
            resources_dict,
            form_xobject.data.len(),
        )
        .as_bytes(),
    );
    buffer.extend_from_slice(&form_xobject.data);
    buffer.extend_from_slice(b"\nendstream\nendobj\n");
}

fn image_color_space_name(color_space: ImageColorSpace) -> &'static str {
    match color_space {
        ImageColorSpace::DeviceRGB => "DeviceRGB",
        ImageColorSpace::DeviceGray => "DeviceGray",
    }
}

fn image_filter_name(filter: ImageFilter) -> &'static str {
    match filter {
        ImageFilter::DCTDecode => "DCTDecode",
        ImageFilter::FlateDecode => "FlateDecode",
    }
}

fn image_decode_params(image: &PdfImageXObject) -> String {
    if image.filter != ImageFilter::FlateDecode {
        return String::new();
    }

    let colors = match image.color_space {
        ImageColorSpace::DeviceRGB => 3,
        ImageColorSpace::DeviceGray => 1,
    };

    format!(
        " /DecodeParms << /Predictor 15 /Colors {} /BitsPerComponent {} /Columns {} >>",
        colors, image.bits_per_component, image.width
    )
}

fn assign_annotation_object_ids(
    page_annotations: &mut [Vec<PdfLinkAnnotation>],
    start_object_id: usize,
) -> usize {
    let mut next_object_id = start_object_id;
    for annotations in page_annotations {
        for annotation in annotations {
            annotation.object_id = next_object_id;
            next_object_id += 1;
        }
    }
    next_object_id
}

fn page_link_annotations(page: &TypesetPage) -> Vec<PdfLinkAnnotation> {
    page.lines
        .iter()
        .flat_map(line_link_annotations)
        .chain(
            page.float_placements
                .iter()
                .flat_map(resolve_float_lines)
                .flat_map(|line| line_link_annotations(&line)),
        )
        .collect()
}

fn line_link_annotations(line: &TextLine) -> Vec<PdfLinkAnnotation> {
    line.links
        .iter()
        .filter_map(|link| {
            let target = pdf_link_target(&link.url)?;
            (link.start_char < link.end_char).then_some(PdfLinkAnnotation {
                object_id: 0,
                target,
                x_start: points(LEFT_MARGIN_PT + LINK_CHAR_WIDTH_PT * link.start_char as i64),
                x_end: points(LEFT_MARGIN_PT + LINK_CHAR_WIDTH_PT * link.end_char as i64),
                y_bottom: line.y - points(LINK_DESCENT_PT),
                y_top: line.y + points(LINK_HEIGHT_PT - LINK_DESCENT_PT),
            })
        })
        .collect()
}

fn pdf_link_target(url: &str) -> Option<PdfLinkTarget> {
    if let Some(name) = url.strip_prefix('#') {
        return (!name.is_empty()).then_some(PdfLinkTarget::InternalDestination(name.to_string()));
    }

    (!url.is_empty()).then_some(PdfLinkTarget::Uri(url.to_string()))
}

fn build_outline_objects(
    outlines: &[TypesetOutline],
    page_object_start: usize,
    root_object_id: usize,
    first_object_id: usize,
) -> OutlineBuildResult {
    if outlines.is_empty() {
        return OutlineBuildResult::default();
    }

    let mut parents = vec![None; outlines.len()];
    for index in 0..outlines.len() {
        parents[index] = (0..index)
            .rev()
            .find(|&candidate| outlines[candidate].level < outlines[index].level);
    }

    let mut root_children = Vec::new();
    let mut children = vec![Vec::new(); outlines.len()];
    for (index, parent) in parents.iter().enumerate() {
        if let Some(parent_index) = parent {
            children[*parent_index].push(index);
        } else {
            root_children.push(index);
        }
    }

    let mut descendant_counts = vec![0usize; outlines.len()];
    for index in (0..outlines.len()).rev() {
        descendant_counts[index] = children[index]
            .iter()
            .map(|&child_index| descendant_counts[child_index] + 1)
            .sum();
    }

    let objects = outlines
        .iter()
        .enumerate()
        .map(|(index, outline)| {
            let object_id = first_object_id + index;
            let parent_object_id = parents[index]
                .map(|parent_index| first_object_id + parent_index)
                .unwrap_or(root_object_id);
            let siblings = parents[index]
                .map(|parent_index| children[parent_index].as_slice())
                .unwrap_or(root_children.as_slice());
            let sibling_position = siblings
                .iter()
                .position(|&candidate| candidate == index)
                .expect("outline should be present among its siblings");
            let prev =
                (sibling_position > 0).then(|| first_object_id + siblings[sibling_position - 1]);
            let next = (sibling_position + 1 < siblings.len())
                .then(|| first_object_id + siblings[sibling_position + 1]);
            let first_child = children[index]
                .first()
                .copied()
                .map(|child_index| first_object_id + child_index);
            let last_child = children[index]
                .last()
                .copied()
                .map(|child_index| first_object_id + child_index);
            let page_object_id = page_object_start + outline.page_index;

            let mut body = format!(
                "<< /Title ({}) /Parent {} 0 R",
                encode_pdf_text(&outline.title).encoded,
                parent_object_id,
            );
            if let Some(prev_id) = prev {
                body.push_str(&format!(" /Prev {prev_id} 0 R"));
            }
            if let Some(next_id) = next {
                body.push_str(&format!(" /Next {next_id} 0 R"));
            }
            if let Some(first_child_id) = first_child {
                body.push_str(&format!(" /First {first_child_id} 0 R"));
            }
            if let Some(last_child_id) = last_child {
                body.push_str(&format!(" /Last {last_child_id} 0 R"));
            }
            if descendant_counts[index] > 0 {
                body.push_str(&format!(" /Count {}", descendant_counts[index]));
            }
            body.push_str(&format!(
                " /Dest [{} 0 R /XYZ {} {} 0] >>",
                page_object_id,
                LEFT_MARGIN_PT,
                points_to_pdf_number(outline.y),
            ));

            OutlineObject { object_id, body }
        })
        .collect();

    OutlineBuildResult {
        objects,
        root_child_object_ids: root_children
            .into_iter()
            .map(|index| first_object_id + index)
            .collect(),
    }
}

fn build_named_destination_object(
    document: &TypesetDocument,
    page_object_start: usize,
    object_id: usize,
) -> String {
    let names = document
        .named_destinations
        .iter()
        .map(|destination| {
            format!(
                "({}) [{} 0 R /XYZ {} {} 0]",
                encode_pdf_text(&destination.name).encoded,
                page_object_start + destination.page_index,
                LEFT_MARGIN_PT,
                points_to_pdf_number(destination.y),
            )
        })
        .collect::<Vec<_>>()
        .join(" ");

    format!("{object_id} 0 obj\n<< /Names [{names}] >>\nendobj\n")
}

fn build_info_dictionary(document: &TypesetDocument) -> Option<String> {
    let mut fields = Vec::new();

    if let Some(title) = document
        .navigation
        .metadata
        .title
        .as_deref()
        .filter(|title| !title.is_empty())
    {
        fields.push(format!("/Title ({})", encode_pdf_text(title).encoded));
    }
    if let Some(author) = document
        .navigation
        .metadata
        .author
        .as_deref()
        .filter(|author| !author.is_empty())
    {
        fields.push(format!("/Author ({})", encode_pdf_text(author).encoded));
    }

    (!fields.is_empty()).then(|| format!("<< {} >>", fields.join(" ")))
}

fn points_to_pdf_number(value: DimensionValue) -> i64 {
    value.0 / SCALED_POINTS_PER_POINT
}

fn dimension_to_pdf_number(value: DimensionValue) -> f64 {
    value.0 as f64 / SCALED_POINTS_PER_POINT as f64
}

fn pdf_real(value: f64) -> String {
    let formatted = format!("{value:.4}");
    formatted
        .trim_end_matches('0')
        .trim_end_matches('.')
        .to_string()
}

fn points(value: i64) -> DimensionValue {
    DimensionValue(value * SCALED_POINTS_PER_POINT)
}

fn append_object(
    buffer: &mut Vec<u8>,
    offsets: &mut BTreeMap<usize, usize>,
    object_id: usize,
    object: &str,
) {
    append_object_bytes(buffer, offsets, object_id, object.as_bytes());
}

fn append_object_bytes(
    buffer: &mut Vec<u8>,
    offsets: &mut BTreeMap<usize, usize>,
    object_id: usize,
    object: &[u8],
) {
    record_object_offset(offsets, object_id, buffer.len());
    buffer.extend_from_slice(object);
}

fn record_object_offset(
    offsets: &mut BTreeMap<usize, usize>,
    object_id: usize,
    byte_offset: usize,
) {
    let previous = offsets.insert(object_id, byte_offset);
    debug_assert!(
        previous.is_none(),
        "duplicate PDF object id {object_id}: previous offset {previous:?}, new offset {byte_offset}"
    );
}

fn unicode_to_winansi(ch: char) -> Option<u8> {
    match ch {
        '\u{20AC}' => Some(0x80),
        '\u{201A}' => Some(0x82),
        '\u{0192}' => Some(0x83),
        '\u{201E}' => Some(0x84),
        '\u{2026}' => Some(0x85),
        '\u{2020}' => Some(0x86),
        '\u{2021}' => Some(0x87),
        '\u{02C6}' => Some(0x88),
        '\u{2030}' => Some(0x89),
        '\u{0160}' => Some(0x8A),
        '\u{2039}' => Some(0x8B),
        '\u{0152}' => Some(0x8C),
        '\u{017D}' => Some(0x8E),
        '\u{2018}' => Some(0x91),
        '\u{2019}' => Some(0x92),
        '\u{201C}' => Some(0x93),
        '\u{201D}' => Some(0x94),
        '\u{2022}' => Some(0x95),
        '\u{2013}' => Some(0x96),
        '\u{2014}' => Some(0x97),
        '\u{02DC}' => Some(0x98),
        '\u{2122}' => Some(0x99),
        '\u{0161}' => Some(0x9A),
        '\u{203A}' => Some(0x9B),
        '\u{0153}' => Some(0x9C),
        '\u{017E}' => Some(0x9E),
        '\u{0178}' => Some(0x9F),
        _ => {
            let code_point = u32::from(ch);
            ((0x20..=0x7E).contains(&code_point) || (0xA0..=0xFF).contains(&code_point))
                .then_some(code_point as u8)
        }
    }
}

fn encode_pdf_text(value: &str) -> PdfTextEncodeResult {
    let mut result = String::with_capacity(value.len());
    let mut unencodable_chars = Vec::new();
    for ch in value.chars() {
        if matches!(ch, '\r' | '\n' | '\t') {
            result.push(' ');
            continue;
        }

        match unicode_to_winansi(ch) {
            Some(b'\\') => result.push_str("\\\\"),
            Some(b'(') => result.push_str("\\("),
            Some(b')') => result.push_str("\\)"),
            Some(byte @ 0x20..=0x7E) => result.push(byte as char),
            Some(byte) => result.push_str(&format!("\\{:03o}", byte)),
            None => {
                result.push('?');
                unencodable_chars.push(ch);
            }
        }
    }

    PdfTextEncodeResult {
        encoded: result,
        unencodable_chars,
    }
}

fn encode_pdf_actual_text(value: &str) -> String {
    if value
        .chars()
        .all(|ch| matches!(ch, '\r' | '\n' | '\t') || unicode_to_winansi(ch).is_some())
    {
        return format!("({})", encode_pdf_text(value).encoded);
    }

    let mut encoded = String::from("<FEFF");
    for ch in value.chars() {
        let mut code_units = [0u16; 2];
        for unit in ch.encode_utf16(&mut code_units).iter() {
            encoded.push_str(&format!("{unit:04X}"));
        }
    }
    encoded.push('>');
    encoded
}

/// Maps a Unicode math/Greek codepoint to a byte in the built-in Symbol Type1 font
/// encoding (Adobe Symbol character set). Returns `None` for code points that are
/// not covered by the Symbol font.
fn unicode_to_symbol_byte(ch: char) -> Option<u8> {
    match ch {
        // Greek lowercase
        '\u{03B1}' => Some(0x61),              // α
        '\u{03B2}' => Some(0x62),              // β
        '\u{03B3}' => Some(0x67),              // γ
        '\u{03B4}' => Some(0x64),              // δ
        '\u{03B5}' | '\u{03F5}' => Some(0x65), // ε / ϵ
        '\u{03B6}' => Some(0x7A),              // ζ
        '\u{03B7}' => Some(0x68),              // η
        '\u{03B8}' => Some(0x71),              // θ
        '\u{03D1}' => Some(0x4A),              // ϑ (theta1)
        '\u{03B9}' => Some(0x69),              // ι
        '\u{03BA}' => Some(0x6B),              // κ
        '\u{03BB}' => Some(0x6C),              // λ
        '\u{03BC}' => Some(0x6D),              // μ
        '\u{03BD}' => Some(0x6E),              // ν
        '\u{03BE}' => Some(0x78),              // ξ
        '\u{03BF}' => Some(0x6F),              // ο
        '\u{03C0}' => Some(0x70),              // π
        '\u{03D6}' => Some(0x76),              // ϖ (varpi)
        '\u{03C1}' => Some(0x72),              // ρ
        '\u{03C3}' => Some(0x73),              // σ
        '\u{03C2}' => Some(0x56),              // ς (final sigma) → Symbol's sigma1 at 'V'
        '\u{03C4}' => Some(0x74),              // τ
        '\u{03C5}' => Some(0x75),              // υ
        '\u{03C6}' => Some(0x66),              // φ (phi)
        '\u{03D5}' => Some(0x6A),              // ϕ (phi1 variant)
        '\u{03C7}' => Some(0x63),              // χ
        '\u{03C8}' => Some(0x79),              // ψ
        '\u{03C9}' => Some(0x77),              // ω

        // Greek uppercase
        '\u{0391}' => Some(0x41),              // Α
        '\u{0392}' => Some(0x42),              // Β
        '\u{0393}' => Some(0x47),              // Γ
        '\u{0394}' => Some(0x44),              // Δ
        '\u{0395}' => Some(0x45),              // Ε
        '\u{0396}' => Some(0x5A),              // Ζ
        '\u{0397}' => Some(0x48),              // Η
        '\u{0398}' => Some(0x51),              // Θ
        '\u{0399}' => Some(0x49),              // Ι
        '\u{039A}' => Some(0x4B),              // Κ
        '\u{039B}' => Some(0x4C),              // Λ
        '\u{039C}' => Some(0x4D),              // Μ
        '\u{039D}' => Some(0x4E),              // Ν
        '\u{039E}' => Some(0x58),              // Ξ
        '\u{039F}' => Some(0x4F),              // Ο
        '\u{03A0}' => Some(0x50),              // Π
        '\u{03A1}' => Some(0x52),              // Ρ
        '\u{03A3}' => Some(0x53),              // Σ
        '\u{03A4}' => Some(0x54),              // Τ
        '\u{03A5}' => Some(0x55),              // Υ
        '\u{03D2}' => Some(0xA1),              // ϒ (Upsilon1)
        '\u{03A6}' => Some(0x46),              // Φ
        '\u{03A7}' => Some(0x43),              // Χ
        '\u{03A8}' => Some(0x59),              // Ψ
        '\u{03A9}' | '\u{2126}' => Some(0x57), // Ω / ohm sign

        // Math operators and relations
        '\u{2200}' => Some(0x22), // ∀
        '\u{2203}' => Some(0x24), // ∃
        '\u{2205}' => Some(0xC6), // ∅
        '\u{2207}' => Some(0xD1), // ∇
        '\u{2202}' => Some(0xB6), // ∂
        '\u{2208}' => Some(0xCE), // ∈
        '\u{2209}' => Some(0xCF), // ∉
        '\u{221D}' => Some(0xB5), // ∝
        '\u{221E}' => Some(0xA5), // ∞
        '\u{2220}' => Some(0xD0), // ∠
        '\u{2227}' => Some(0xD9), // ∧
        '\u{2228}' => Some(0xDA), // ∨
        '\u{2229}' => Some(0xC7), // ∩
        '\u{222A}' => Some(0xC8), // ∪
        '\u{222B}' => Some(0xF2), // ∫
        '\u{2320}' => Some(0xF3), // ⌠
        '\u{2321}' => Some(0xF5), // ⌡
        '\u{223C}' => Some(0x7E), // ∼
        '\u{2245}' => Some(0x40), // ≅
        '\u{2248}' => Some(0xBB), // ≈
        '\u{2260}' => Some(0xB9), // ≠
        '\u{2261}' => Some(0xBA), // ≡
        '\u{2264}' => Some(0xA3), // ≤
        '\u{2265}' => Some(0xB3), // ≥
        '\u{2282}' => Some(0xCC), // ⊂
        '\u{2283}' => Some(0xC9), // ⊃
        '\u{2284}' => Some(0xCB), // ⊄
        '\u{2286}' => Some(0xCD), // ⊆
        '\u{2287}' => Some(0xCA), // ⊇
        '\u{2295}' => Some(0xC5), // ⊕
        '\u{2297}' => Some(0xC4), // ⊗
        '\u{22A5}' => Some(0x5E), // ⊥
        '\u{22C5}' => Some(0xD7), // ⋅
        '\u{221A}' => Some(0xD6), // √
        '\u{2211}' => Some(0xE5), // ∑
        '\u{220F}' => Some(0xD5), // ∏
        '\u{00B0}' => Some(0xB0), // °
        '\u{00B1}' => Some(0xB1), // ±
        '\u{00D7}' => Some(0xB4), // ×
        '\u{00F7}' => Some(0xB8), // ÷
        '\u{00AC}' => Some(0xD8), // ¬

        // Arrows
        '\u{2190}' => Some(0xAC), // ←
        '\u{2191}' => Some(0xAD), // ↑
        '\u{2192}' => Some(0xAE), // →
        '\u{2193}' => Some(0xAF), // ↓
        '\u{2194}' => Some(0xAB), // ↔
        '\u{21D0}' => Some(0xDC), // ⇐
        '\u{21D1}' => Some(0xDD), // ⇑
        '\u{21D2}' => Some(0xDE), // ⇒
        '\u{21D3}' => Some(0xDF), // ⇓
        '\u{21D4}' => Some(0xDB), // ⇔

        // Delimiters and brackets
        '\u{27E8}' => Some(0xE1), // ⟨
        '\u{27E9}' => Some(0xF1), // ⟩
        '\u{2308}' => Some(0xE9), // ⌈
        '\u{2309}' => Some(0xF9), // ⌉
        '\u{230A}' => Some(0xEB), // ⌊
        '\u{230B}' => Some(0xFB), // ⌋

        // Misc math symbols
        '\u{2135}' => Some(0xC0), // ℵ
        '\u{2111}' => Some(0xC1), // ℑ
        '\u{211C}' => Some(0xC2), // ℜ
        '\u{2118}' => Some(0xC3), // ℘
        '\u{2032}' => Some(0xA2), // ′
        '\u{2026}' => Some(0xBC), // … (ellipsis)

        _ => None,
    }
}

/// Appends one PDF literal-string byte to `out`, escaping specials and
/// encoding non-printables as octal sequences.
fn append_pdf_string_byte(byte: u8, out: &mut String) {
    match byte {
        b'\\' => out.push_str("\\\\"),
        b'(' => out.push_str("\\("),
        b')' => out.push_str("\\)"),
        0x20..=0x7E => out.push(byte as char),
        _ => out.push_str(&format!("\\{:03o}", byte)),
    }
}

/// Slot describing which PDF font a run of bytes should be written with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PdfTextFontSlot {
    /// The line's configured primary text font (WinAnsi encoded).
    Primary,
    /// The Symbol Type1 font (built-in Symbol encoding, covers Greek/math glyphs).
    Symbol,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PdfTextRun {
    slot: PdfTextFontSlot,
    encoded: String,
    unencodable_chars: Vec<char>,
}

/// Splits `value` into PDF text runs, routing WinAnsi-representable code points
/// through the primary font and Greek/math code points through the Symbol font.
/// Thin space (U+2009) is folded to regular space to avoid spurious errors
/// from a codepoint the Symbol font does not cover.
fn split_into_font_runs(value: &str) -> Vec<PdfTextRun> {
    let mut runs: Vec<PdfTextRun> = Vec::new();

    let push_byte = |runs: &mut Vec<PdfTextRun>, slot: PdfTextFontSlot, byte: u8| {
        if let Some(last) = runs.last_mut() {
            if last.slot == slot {
                append_pdf_string_byte(byte, &mut last.encoded);
                return;
            }
        }
        let mut encoded = String::new();
        append_pdf_string_byte(byte, &mut encoded);
        runs.push(PdfTextRun {
            slot,
            encoded,
            unencodable_chars: Vec::new(),
        });
    };

    let push_unencodable = |runs: &mut Vec<PdfTextRun>, ch: char| {
        if let Some(last) = runs.last_mut() {
            if last.slot == PdfTextFontSlot::Primary {
                last.encoded.push('?');
                last.unencodable_chars.push(ch);
                return;
            }
        }
        runs.push(PdfTextRun {
            slot: PdfTextFontSlot::Primary,
            encoded: "?".to_string(),
            unencodable_chars: vec![ch],
        });
    };

    for ch in value.chars() {
        if matches!(ch, '\r' | '\n' | '\t' | '\u{2009}') {
            push_byte(&mut runs, PdfTextFontSlot::Primary, b' ');
            continue;
        }

        if let Some(byte) = unicode_to_winansi(ch) {
            push_byte(&mut runs, PdfTextFontSlot::Primary, byte);
            continue;
        }

        if let Some(byte) = unicode_to_symbol_byte(ch) {
            push_byte(&mut runs, PdfTextFontSlot::Symbol, byte);
            continue;
        }

        push_unencodable(&mut runs, ch);
    }

    runs
}

/// Collects all characters that remain unrepresentable even after the Symbol
/// font fallback. These are the only characters that produce user-visible
/// encoding errors.
fn collect_unencodable_chars(value: &str, encoding_error_chars: &mut BTreeSet<char>) {
    for run in split_into_font_runs(value) {
        encoding_error_chars.extend(run.unencodable_chars);
    }
}

/// Emits one line's worth of text content as PDF text showing operators,
/// switching between the primary font (`primary_font_number`) and the Symbol
/// font (`math_font_number`) as needed so that math-mode Unicode glyphs are
/// rendered with an appropriate font rather than being dropped to `?`.
///
/// Assumes the current text font when the function is entered is the primary
/// font at `font_size`. On exit, the current font is restored to the primary
/// font at `font_size`.
fn emit_text_with_font_runs(
    stream: &mut String,
    value: &str,
    primary_font_number: usize,
    math_font_number: usize,
    font_size: DimensionValue,
    warning_chars: &mut BTreeSet<char>,
) {
    let runs = split_into_font_runs(value);
    if runs.is_empty() {
        stream.push_str("() Tj\n");
        return;
    }

    let size_str = points_to_pdf_number(font_size);
    let mut primary_active = true;
    for run in runs {
        warning_chars.extend(run.unencodable_chars);
        match run.slot {
            PdfTextFontSlot::Primary => {
                if !primary_active {
                    stream.push_str(&format!("/F{primary_font_number} {size_str} Tf\n"));
                    primary_active = true;
                }
            }
            PdfTextFontSlot::Symbol => {
                stream.push_str(&format!("/F{math_font_number} {size_str} Tf\n"));
                primary_active = false;
            }
        }
        stream.push_str(&format!("({}) Tj\n", run.encoded));
    }

    if !primary_active {
        stream.push_str(&format!("/F{primary_font_number} {size_str} Tf\n"));
    }
}

fn pdf_encoding_error(ch: char) -> String {
    format!(
        "PDF encoding: character '{}' (U+{:04X}) is not supported by the current font stack (WinAnsi + Symbol) and was replaced with '?'. Ferritex does not yet support non-Latin Unicode code points (e.g. CJK) outside this set.",
        ch,
        u32::from(ch)
    )
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeMap, BTreeSet};

    use super::{
        encode_pdf_actual_text, encode_pdf_text, opacity_graphics_state_name,
        render_vector_primitive, resolve_named_color, unicode_to_winansi, FontResource,
        ImageColorSpace, ImageFilter, OpacityGraphicsStateKey, PageRenderPayload, PdfFormXObject,
        PdfImageXObject, PdfLinkAnnotation, PdfLinkTarget, PdfRenderer, PlacedFormXObject,
        PlacedImage,
    };
    use crate::assets::api::{AssetHandle, LogicalAssetId};
    use crate::compilation::{
        DocumentPartitionPlan, DocumentWorkUnit, LinkStyle, PartitionKind, PartitionLocator,
    };
    use crate::graphics::api::{
        ArrowSpec, Color, DashPattern, ExternalGraphic, GraphicGroup, GraphicNode, GraphicText,
        GraphicsScene, ImageMetadata, LineCap, LineJoin, PathSegment, PdfGraphic,
        PdfGraphicMetadata, Point, Transform2D, VectorPrimitive,
    };
    use crate::kernel::api::{DimensionValue, StableId};
    use crate::typesetting::api::{
        FloatPlacement, PageBox, TextLine, TextLineLink, TypesetDocument, TypesetImage,
        TypesetNamedDestination, TypesetOutline, TypesetPage,
    };

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
                    x: DimensionValue::zero(),
                    y: points(720 - (index as i64 * 18)),
                    links: Vec::new(),
                    font_index: 0,
                    font_size: points(10),
                    source_span: None,
                })
                .collect(),
            images: Vec::new(),
            float_placements: Vec::new(),
            index_entries: Vec::new(),
        }
    }

    fn single_page(lines: &[&str]) -> TypesetDocument {
        TypesetDocument {
            pages: vec![page(lines)],
            outlines: Vec::new(),
            named_destinations: Vec::new(),
            title: None,
            author: None,
            navigation: Default::default(),
            index_entries: Vec::new(),
            has_unresolved_index: false,
        }
    }

    fn single_page_with_images(lines: &[&str], images: Vec<TypesetImage>) -> TypesetDocument {
        let mut document = single_page(lines);
        document.pages[0].images = images;
        document
    }

    fn actual_text_payloads<'a>(content: &'a str) -> Vec<&'a str> {
        let mut payloads = Vec::new();
        let marker = "/ActualText ";
        let mut remaining = content;

        while let Some(start) = remaining.find(marker) {
            let payload_source = &remaining[start + marker.len()..];
            let Some(first) = payload_source.chars().next() else {
                break;
            };

            let payload_len = match first {
                '(' => {
                    let mut escaped = false;
                    let mut depth = 0usize;
                    let mut end = None;
                    for (offset, ch) in payload_source.char_indices() {
                        if escaped {
                            escaped = false;
                            continue;
                        }
                        match ch {
                            '\\' => escaped = true,
                            '(' => depth += 1,
                            ')' => {
                                depth = depth.saturating_sub(1);
                                if depth == 0 {
                                    end = Some(offset + ch.len_utf8());
                                    break;
                                }
                            }
                            _ => {}
                        }
                    }
                    end.expect("unterminated literal ActualText")
                }
                '<' => payload_source
                    .find('>')
                    .map(|index| index + 1)
                    .expect("unterminated hex ActualText"),
                _ => panic!("unexpected ActualText payload start: {first}"),
            };

            payloads.push(&payload_source[..payload_len]);
            remaining = &payload_source[payload_len..];
        }

        payloads
    }

    fn raster_scene() -> GraphicsScene {
        GraphicsScene {
            nodes: vec![GraphicNode::External(ExternalGraphic {
                path: "image.png".to_string(),
                asset_handle: AssetHandle {
                    id: LogicalAssetId(StableId(1)),
                },
                metadata: ImageMetadata {
                    width: 1,
                    height: 1,
                    color_space: ImageColorSpace::DeviceRGB,
                    bits_per_component: 8,
                },
            })],
        }
    }

    fn pdf_scene() -> GraphicsScene {
        GraphicsScene {
            nodes: vec![GraphicNode::Pdf(PdfGraphic {
                path: "figure.pdf".to_string(),
                asset_handle: AssetHandle {
                    id: LogicalAssetId(StableId(2)),
                },
                metadata: PdfGraphicMetadata {
                    media_box: [0.0, 0.0, 200.0, 100.0],
                    page_data: b"0 0 m\n200 100 l\nS".to_vec(),
                    resources_dict: Some("<< /ProcSet [/PDF] >>".to_string()),
                },
            })],
        }
    }

    fn vector_scene() -> GraphicsScene {
        GraphicsScene {
            nodes: vec![GraphicNode::Vector(VectorPrimitive {
                path: vec![
                    PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                    PathSegment::LineTo(Point { x: 20.0, y: 0.0 }),
                    PathSegment::LineTo(Point { x: 20.0, y: 20.0 }),
                    PathSegment::ClosePath,
                ],
                stroke: Some(Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                }),
                fill: Some(Color {
                    r: 0.0,
                    g: 0.0,
                    b: 1.0,
                }),
                line_width: 0.4,
                ..Default::default()
            })],
        }
    }

    fn grouped_vector_scene() -> GraphicsScene {
        GraphicsScene {
            nodes: vec![GraphicNode::Group(GraphicGroup {
                children: vec![GraphicNode::Vector(VectorPrimitive {
                    path: vec![
                        PathSegment::MoveTo(Point { x: 1.0, y: 1.0 }),
                        PathSegment::LineTo(Point { x: 8.0, y: 8.0 }),
                    ],
                    stroke: Some(Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                    }),
                    fill: None,
                    line_width: 0.4,
                    ..Default::default()
                })],
                default_stroke: Some(Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                }),
                default_fill: None,
                default_line_width: Some(0.4),
                clip_path: Some(vec![
                    PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                    PathSegment::LineTo(Point { x: 10.0, y: 0.0 }),
                    PathSegment::LineTo(Point { x: 10.0, y: 10.0 }),
                    PathSegment::ClosePath,
                ]),
                transform: Transform2D {
                    x_shift: 5.0,
                    y_shift: 7.0,
                    scale: 2.0,
                    rotate: 90.0,
                },
            })],
        }
    }

    fn arrow_scene(arrows: ArrowSpec) -> GraphicsScene {
        GraphicsScene {
            nodes: vec![GraphicNode::Vector(VectorPrimitive {
                path: vec![
                    PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                    PathSegment::LineTo(Point { x: 20.0, y: 0.0 }),
                ],
                stroke: Some(Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                }),
                fill: None,
                line_width: 0.4,
                arrows,
                ..Default::default()
            })],
        }
    }

    fn text_scene() -> GraphicsScene {
        GraphicsScene {
            nodes: vec![GraphicNode::Text(GraphicText {
                position: Point { x: 10.0, y: 12.0 },
                content: "Hello".to_string(),
            })],
        }
    }

    fn linked_line(text: &str, start_char: usize, end_char: usize, url: &str) -> TextLine {
        TextLine {
            text: text.to_string(),
            x: DimensionValue::zero(),
            y: points(720),
            links: vec![TextLineLink {
                url: url.to_string(),
                start_char,
                end_char,
            }],
            font_index: 0,
            font_size: points(10),
            source_span: None,
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
    fn renders_float_placement_text() {
        let mut page = page(&["Main"]);
        page.float_placements.push(FloatPlacement {
            region: crate::typesetting::api::FloatRegion::Here,
            content: crate::typesetting::api::FloatContent {
                lines: vec![TextLine {
                    text: "Float text".to_string(),
                    x: DimensionValue::zero(),
                    y: points(0),
                    links: Vec::new(),
                    font_index: 0,
                    font_size: points(10),
                    source_span: None,
                }],
                images: Vec::new(),
                height: points(18),
            },
            y_position: points(680),
        });

        let pdf = PdfRenderer::default().render(&TypesetDocument {
            pages: vec![page],
            outlines: Vec::new(),
            named_destinations: Vec::new(),
            title: None,
            author: None,
            navigation: Default::default(),
            index_entries: Vec::new(),
            has_unresolved_index: false,
        });
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("Float text"));
    }

    #[test]
    fn renders_script_lines_with_text_rise_and_actual_text() {
        let superscripted = format!(
            "Inline x{}2{} y{}1{} note{}3{}",
            crate::typesetting::math_layout::SUPERSCRIPT_START_MARKER,
            crate::typesetting::math_layout::SUPERSCRIPT_END_MARKER,
            crate::typesetting::math_layout::SUBSCRIPT_START_MARKER,
            crate::typesetting::math_layout::SUBSCRIPT_END_MARKER,
            crate::typesetting::api::FOOTNOTE_MARKER_START,
            crate::typesetting::api::FOOTNOTE_MARKER_END,
        );
        let mut document = single_page(&[]);
        document.pages[0].lines = vec![TextLine {
            text: superscripted,
            x: DimensionValue::zero(),
            y: points(720),
            links: Vec::new(),
            font_index: 0,
            font_size: points(10),
            source_span: None,
        }];

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/Span <</ActualText (Inline x2 y1 note3)>> BDC"));
        assert!(content.contains("4 Ts"));
        assert!(content.contains("-3 Ts"));
        assert!(content.contains("\n3 Ts\n"));
        assert_eq!(content.matches("0 Ts").count(), 3);
    }

    #[test]
    fn tracks_page_count_for_multi_page_documents() {
        let document = TypesetDocument {
            pages: vec![page(&["Page 1"]), page(&["Page 2"])],
            outlines: Vec::new(),
            named_destinations: Vec::new(),
            title: None,
            author: None,
            navigation: Default::default(),
            index_entries: Vec::new(),
            has_unresolved_index: false,
        };
        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert_eq!(pdf.page_count, 2);
        assert!(content.contains("/Count 2"));
    }

    #[test]
    fn info_dictionary_uses_navigation_metadata() {
        let mut document = single_page(&["Metadata"]);
        document.title = Some("Legacy Title".to_string());
        document.author = Some("Legacy Author".to_string());
        document.navigation.metadata.title = Some("Navigation Title".to_string());
        document.navigation.metadata.author = Some("Navigation Author".to_string());

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/Title (Navigation Title)"));
        assert!(content.contains("/Author (Navigation Author)"));
        assert!(!content.contains("/Title (Legacy Title)"));
        assert!(!content.contains("/Author (Legacy Author)"));
    }

    #[test]
    fn emits_image_xobject_and_placement_commands() {
        let document = single_page_with_images(
            &[],
            vec![TypesetImage {
                scene: raster_scene(),
                x: points(72),
                y: points(600),
                display_width: points(100),
                display_height: points(100),
            }],
        );
        let renderer = PdfRenderer::default().with_images(
            vec![PdfImageXObject {
                object_id: 0,
                width: 1,
                height: 1,
                color_space: ImageColorSpace::DeviceRGB,
                bits_per_component: 8,
                data: vec![120, 156, 99, 248, 207, 192, 0, 0, 3, 1, 1, 0],
                filter: ImageFilter::FlateDecode,
            }],
            vec![vec![PlacedImage {
                xobject_index: 0,
                x: points(72),
                y: points(600),
                display_width: points(100),
                display_height: points(100),
            }]],
        );
        let pdf = renderer.render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/XObject << /Im1"));
        assert!(content.contains("/Subtype /Image /Width 1 /Height 1"));
        assert!(content.contains("/Filter /FlateDecode"));
        assert!(content
            .contains("/DecodeParms << /Predictor 15 /Colors 3 /BitsPerComponent 8 /Columns 1 >>"));
        assert!(content.contains("q 100 0 0 100 72 600 cm /Im1 Do Q"));
    }

    #[test]
    fn emits_form_xobject_and_placement_commands() {
        let document = single_page_with_images(
            &[],
            vec![TypesetImage {
                scene: pdf_scene(),
                x: points(72),
                y: points(600),
                display_width: points(100),
                display_height: points(50),
            }],
        );
        let renderer = PdfRenderer::default().with_form_xobjects(
            vec![PdfFormXObject {
                object_id: 0,
                media_box: [0.0, 0.0, 200.0, 100.0],
                data: b"0 0 m\n200 100 l\nS".to_vec(),
                resources_dict: Some("<< /ProcSet [/PDF] >>".to_string()),
            }],
            vec![vec![PlacedFormXObject {
                xobject_index: 0,
                x: points(72),
                y: points(600),
                display_width: points(100),
                display_height: points(50),
            }]],
        );
        let pdf = renderer.render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/XObject << /Fm1"));
        assert!(content.contains("/Subtype /Form"));
        assert!(content.contains("/BBox [0 0 200 100]"));
        assert!(content.contains("/Resources << /ProcSet [/PDF] >>"));
        assert!(content.contains("0 0 m\n200 100 l\nS"));
        assert!(content.contains("q 0.5 0 0 0.5 72 600 cm /Fm1 Do Q"));
    }

    #[test]
    fn emits_vector_graphics_pdf_operators() {
        let document = single_page_with_images(
            &[],
            vec![TypesetImage {
                scene: vector_scene(),
                x: points(72),
                y: points(600),
                display_width: points(20),
                display_height: points(20),
            }],
        );

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("q 1 0 0 1 72 600 cm"));
        assert!(content.contains("0.4 w"));
        assert!(content.contains("0 0 0 RG"));
        assert!(content.contains("0 0 1 rg"));
        assert!(content.contains("0 0 m"));
        assert!(content.contains("20 0 l"));
        assert!(content.contains("20 20 l"));
        assert!(content.contains("h"));
        assert!(!content.contains("0 J"));
        assert!(!content.contains("0 j"));
        assert!(content.contains("B"));
    }

    #[test]
    fn emits_tikz_standard_dash_arrays_for_vector_primitives() {
        let expectations = [
            (DashPattern::Dashed, "[3 3] 0 d"),
            (DashPattern::LooselyDashed, "[3 6] 0 d"),
            (DashPattern::DashDot, "[3 2 1 2] 0 d"),
            (DashPattern::DashDotDot, "[3 2 1 2 1 2] 0 d"),
        ];

        for (dash_pattern, expected_operator) in expectations {
            let content = render_vector_primitive(&VectorPrimitive {
                path: vec![
                    PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                    PathSegment::LineTo(Point { x: 20.0, y: 0.0 }),
                ],
                dash_pattern,
                ..Default::default()
            });

            assert!(content.contains(expected_operator));
        }
    }

    #[test]
    fn emits_dash_cap_join_and_opacity_operators_for_vector_primitives() {
        let key = OpacityGraphicsStateKey::new(0.25, 0.5).expect("non-default opacity key");
        let resource_name = opacity_graphics_state_name(key);
        let document = single_page_with_images(
            &[],
            vec![TypesetImage {
                scene: GraphicsScene {
                    nodes: vec![GraphicNode::Vector(VectorPrimitive {
                        path: vec![
                            PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                            PathSegment::LineTo(Point { x: 20.0, y: 0.0 }),
                        ],
                        stroke: Some(Color {
                            r: 0.0,
                            g: 0.0,
                            b: 0.0,
                        }),
                        fill: Some(Color {
                            r: 0.0,
                            g: 0.0,
                            b: 1.0,
                        }),
                        line_width: 0.4,
                        dash_pattern: DashPattern::DashDot,
                        line_cap: LineCap::Round,
                        line_join: LineJoin::Bevel,
                        opacity: 0.25,
                        fill_opacity: 0.5,
                        ..Default::default()
                    })],
                },
                x: points(72),
                y: points(600),
                display_width: points(20),
                display_height: points(20),
            }],
        );

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("[3 2 1 2] 0 d"));
        assert!(content.contains("1 J"));
        assert!(content.contains("2 j"));
        assert!(content.contains(&format!("/{resource_name} gs")));
        assert!(content.contains(&format!("/ExtGState << /{resource_name}")));
        assert!(content.contains("/Type /ExtGState /CA 0.25 /ca 0.5"));
    }

    #[test]
    fn deduplicates_opacity_graphics_state_resources_per_page() {
        let key = OpacityGraphicsStateKey::new(0.25, 0.5).expect("non-default opacity key");
        let resource_name = opacity_graphics_state_name(key);
        let scene = GraphicsScene {
            nodes: vec![GraphicNode::Vector(VectorPrimitive {
                path: vec![
                    PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                    PathSegment::LineTo(Point { x: 20.0, y: 0.0 }),
                ],
                stroke: Some(Color {
                    r: 0.0,
                    g: 0.0,
                    b: 0.0,
                }),
                fill: None,
                line_width: 0.4,
                opacity: 0.25,
                fill_opacity: 0.5,
                ..Default::default()
            })],
        };
        let document = single_page_with_images(
            &[],
            vec![
                TypesetImage {
                    scene: scene.clone(),
                    x: points(72),
                    y: points(600),
                    display_width: points(20),
                    display_height: points(20),
                },
                TypesetImage {
                    scene,
                    x: points(100),
                    y: points(560),
                    display_width: points(20),
                    display_height: points(20),
                },
            ],
        );

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains(&format!("/ExtGState << /{resource_name}")));
        assert_eq!(
            content.matches("/Type /ExtGState /CA 0.25 /ca 0.5").count(),
            1
        );
    }

    #[test]
    fn emits_group_transform_and_clip_operators() {
        let document = single_page_with_images(
            &[],
            vec![TypesetImage {
                scene: grouped_vector_scene(),
                x: points(72),
                y: points(600),
                display_width: points(20),
                display_height: points(20),
            }],
        );

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("q 1 0 0 1 72 600 cm"));
        assert!(content.contains("q\n0 2 -2 0 5 7 cm\n"));
        assert!(content.contains("0 0 m\n10 0 l\n10 10 l\nh\nW n"));
        assert!(content.contains("1 1 m\n8 8 l"));
    }

    #[test]
    fn emits_arrowhead_paths_for_vector_primitives() {
        let document = single_page_with_images(
            &[],
            vec![
                TypesetImage {
                    scene: arrow_scene(ArrowSpec::Forward),
                    x: points(72),
                    y: points(600),
                    display_width: points(20),
                    display_height: points(10),
                },
                TypesetImage {
                    scene: arrow_scene(ArrowSpec::Both),
                    x: points(100),
                    y: points(560),
                    display_width: points(20),
                    display_height: points(10),
                },
            ],
        );

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("20 0 m\n16 2 l\n16 -2 l\nh\nf"));
        assert!(content.contains("0 0 m\n4 -2 l\n4 2 l\nh\nf"));
    }

    #[test]
    fn emits_graphic_text_bt_et_block() {
        let document = single_page_with_images(
            &[],
            vec![TypesetImage {
                scene: text_scene(),
                x: points(72),
                y: points(600),
                display_width: points(30),
                display_height: points(12),
            }],
        );

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("BT"));
        assert!(content.contains("/F1 12 Tf"));
        assert!(content.contains("10 12 Td"));
        assert!(content.contains("(Hello) Tj"));
        assert!(content.contains("ET"));
    }

    #[test]
    fn render_with_parallelism_matches_sequential_output() {
        let mut document = single_page(&["Page 1 link", "Page 1 body"]);
        document.pages.push(page(&["Page 2 link", "Page 2 body"]));
        document.pages[0].lines[0].links = vec![TextLineLink {
            url: "https://example.com/one".to_string(),
            start_char: 0,
            end_char: 11,
        }];
        document.pages[1].lines[0].links = vec![TextLineLink {
            url: "https://example.com/two".to_string(),
            start_char: 0,
            end_char: 11,
        }];
        document.pages[0].images.push(TypesetImage {
            scene: raster_scene(),
            x: points(72),
            y: points(600),
            display_width: points(40),
            display_height: points(40),
        });
        document.pages[1].images.push(TypesetImage {
            scene: raster_scene(),
            x: points(120),
            y: points(540),
            display_width: points(60),
            display_height: points(60),
        });
        let renderer = PdfRenderer::default().with_images(
            vec![PdfImageXObject {
                object_id: 0,
                width: 1,
                height: 1,
                color_space: ImageColorSpace::DeviceRGB,
                bits_per_component: 8,
                data: vec![120, 156, 99, 248, 207, 192, 0, 0, 3, 1, 1, 0],
                filter: ImageFilter::FlateDecode,
            }],
            vec![
                vec![PlacedImage {
                    xobject_index: 0,
                    x: points(72),
                    y: points(600),
                    display_width: points(40),
                    display_height: points(40),
                }],
                vec![PlacedImage {
                    xobject_index: 0,
                    x: points(120),
                    y: points(540),
                    display_width: points(60),
                    display_height: points(60),
                }],
            ],
        );

        let sequential = renderer.render(&document);
        let parallel = renderer.render_with_parallelism(&document, 4);

        assert_eq!(parallel.page_count, sequential.page_count);
        assert_eq!(parallel.total_lines, sequential.total_lines);
        assert_eq!(parallel.bytes, sequential.bytes);
    }

    #[test]
    fn render_with_partition_plan_matches_sequential_output() {
        let mut document = single_page(&["1 Intro", "Page 1 body"]);
        document.pages.push(page(&["Page 2 body"]));
        document.pages.push(page(&["2 Results", "Page 3 body"]));
        document.outlines = vec![
            TypesetOutline {
                level: 0,
                title: "1 Intro".to_string(),
                page_index: 0,
                y: points(720),
            },
            TypesetOutline {
                level: 0,
                title: "2 Results".to_string(),
                page_index: 2,
                y: points(720),
            },
        ];
        let plan = DocumentPartitionPlan {
            fallback_partition_id: "document:0000:book".to_string(),
            work_units: vec![
                DocumentWorkUnit {
                    partition_id: "chapter:0001:1-intro".to_string(),
                    kind: PartitionKind::Chapter,
                    locator: PartitionLocator {
                        entry_file: "book.tex".into(),
                        level: 0,
                        ordinal: 0,
                        title: "1 Intro".to_string(),
                    },
                    title: "1 Intro".to_string(),
                },
                DocumentWorkUnit {
                    partition_id: "chapter:0002:2-results".to_string(),
                    kind: PartitionKind::Chapter,
                    locator: PartitionLocator {
                        entry_file: "book.tex".into(),
                        level: 0,
                        ordinal: 1,
                        title: "2 Results".to_string(),
                    },
                    title: "2 Results".to_string(),
                },
            ],
        };
        let renderer = PdfRenderer::default();

        let sequential = renderer.render(&document);
        let parallel = renderer.render_with_partition_plan(&document, 4, 2, &plan, None);

        assert_eq!(parallel.document.page_count, sequential.page_count);
        assert_eq!(parallel.document.total_lines, sequential.total_lines);
        assert_eq!(parallel.document.bytes, sequential.bytes);
    }

    #[test]
    fn render_with_partition_plan_reuses_pre_rendered_page_payloads() {
        let mut document = single_page(&["1 Intro", "Page 1 body"]);
        document.pages.push(page(&["Page 2 body before edit"]));
        document.pages.push(page(&["2 Results", "Page 3 body"]));
        document.outlines = vec![
            TypesetOutline {
                level: 0,
                title: "1 Intro".to_string(),
                page_index: 0,
                y: points(720),
            },
            TypesetOutline {
                level: 0,
                title: "2 Results".to_string(),
                page_index: 2,
                y: points(720),
            },
        ];
        let plan = DocumentPartitionPlan {
            fallback_partition_id: "document:0000:book".to_string(),
            work_units: vec![
                DocumentWorkUnit {
                    partition_id: "chapter:0001:1-intro".to_string(),
                    kind: PartitionKind::Chapter,
                    locator: PartitionLocator {
                        entry_file: "book.tex".into(),
                        level: 0,
                        ordinal: 0,
                        title: "1 Intro".to_string(),
                    },
                    title: "1 Intro".to_string(),
                },
                DocumentWorkUnit {
                    partition_id: "chapter:0002:2-results".to_string(),
                    kind: PartitionKind::Chapter,
                    locator: PartitionLocator {
                        entry_file: "book.tex".into(),
                        level: 0,
                        ordinal: 1,
                        title: "2 Results".to_string(),
                    },
                    title: "2 Results".to_string(),
                },
            ],
        };
        let renderer = PdfRenderer::default();
        let baseline = renderer.render_with_partition_plan(&document, 1, 1, &plan, None);

        let mut edited_document = document.clone();
        edited_document.pages[1] = page(&["Page 2 body after edit"]);
        let overrides = BTreeMap::from([
            (0usize, baseline.page_payloads[0].clone()),
            (2usize, baseline.page_payloads[2].clone()),
        ]);

        let reused =
            renderer.render_with_partition_plan(&edited_document, 1, 1, &plan, Some(&overrides));
        let full = renderer.render_with_partition_plan(&edited_document, 1, 1, &plan, None);

        assert_eq!(reused.document.bytes, full.document.bytes);
        assert_eq!(reused.page_payloads[0], baseline.page_payloads[0]);
        assert_eq!(reused.page_payloads[2], baseline.page_payloads[2]);
        assert_eq!(reused.page_payloads[1], full.page_payloads[1]);
    }

    #[test]
    fn render_with_partition_plan_rerenders_pages_with_xobject_resources() {
        let document = single_page_with_images(
            &[],
            vec![TypesetImage {
                scene: raster_scene(),
                x: points(72),
                y: points(600),
                display_width: points(100),
                display_height: points(100),
            }],
        );
        let renderer = PdfRenderer::default().with_images(
            vec![PdfImageXObject {
                object_id: 0,
                width: 1,
                height: 1,
                color_space: ImageColorSpace::DeviceRGB,
                bits_per_component: 8,
                data: vec![120, 156, 99, 248, 207, 192, 0, 0, 3, 1, 1, 0],
                filter: ImageFilter::FlateDecode,
            }],
            vec![vec![PlacedImage {
                xobject_index: 0,
                x: points(72),
                y: points(600),
                display_width: points(100),
                display_height: points(100),
            }]],
        );
        let full = renderer.render_with_partition_plan(
            &document,
            1,
            1,
            &DocumentPartitionPlan::default(),
            None,
        );
        let bogus_payload = PageRenderPayload::new(
            0,
            Vec::new(),
            BTreeSet::new(),
            "q 1 0 0 1 0 0 cm /Im999 Do Q\n".to_string(),
        );
        let overrides = BTreeMap::from([(0usize, bogus_payload)]);

        let rendered = renderer.render_with_partition_plan(
            &document,
            1,
            1,
            &DocumentPartitionPlan::default(),
            Some(&overrides),
        );

        assert_eq!(rendered.document.bytes, full.document.bytes);
        assert_eq!(rendered.page_payloads[0], full.page_payloads[0]);
        assert_eq!(
            rendered.page_payloads[0].stream,
            "q 100 0 0 100 72 600 cm /Im1 Do Q\n"
        );
    }

    #[test]
    fn render_with_partition_plan_ignores_invalid_cached_page_payload_hash() {
        let document = single_page(&["Payload hash guard"]);
        let renderer = PdfRenderer::default();
        let baseline = renderer.render_with_partition_plan(
            &document,
            1,
            1,
            &DocumentPartitionPlan::default(),
            None,
        );
        let mut invalid_payload = baseline.page_payloads[0].clone();
        invalid_payload.stream.push_str("%tampered\n");
        let overrides = BTreeMap::from([(0usize, invalid_payload)]);

        let rendered = renderer.render_with_partition_plan(
            &document,
            1,
            1,
            &DocumentPartitionPlan::default(),
            Some(&overrides),
        );

        assert_eq!(rendered.document.bytes, baseline.document.bytes);
        assert_eq!(rendered.page_payloads[0], baseline.page_payloads[0]);
    }

    #[test]
    fn page_render_payload_hash_includes_annotations_and_opacity_graphics_states() {
        let base = PageRenderPayload::new(
            0,
            Vec::new(),
            BTreeSet::new(),
            "BT\n(Alpha) Tj\nET\n".to_string(),
        );
        let with_annotation = PageRenderPayload::new(
            0,
            vec![PdfLinkAnnotation {
                object_id: 0,
                target: PdfLinkTarget::Uri("https://example.com".to_string()),
                x_start: points(72),
                x_end: points(144),
                y_bottom: points(690),
                y_top: points(702),
            }],
            BTreeSet::new(),
            "BT\n(Alpha) Tj\nET\n".to_string(),
        );
        let with_opacity = PageRenderPayload::new(
            0,
            Vec::new(),
            BTreeSet::from([OpacityGraphicsStateKey::new(0.5, 0.5).expect("opacity key")]),
            "BT\n(Alpha) Tj\nET\n".to_string(),
        );

        assert_ne!(base.stream_hash, with_annotation.stream_hash);
        assert_ne!(base.stream_hash, with_opacity.stream_hash);
        assert_ne!(with_annotation.stream_hash, with_opacity.stream_hash);
    }

    #[test]
    fn render_outlines_uses_hierarchical_links() {
        let mut document = single_page(&["Outline body"]);
        document.outlines = vec![
            TypesetOutline {
                level: 0,
                title: "Alpha".to_string(),
                page_index: 0,
                y: points(720),
            },
            TypesetOutline {
                level: 1,
                title: "Alpha Sub".to_string(),
                page_index: 0,
                y: points(702),
            },
            TypesetOutline {
                level: 0,
                title: "Beta".to_string(),
                page_index: 0,
                y: points(684),
            },
            TypesetOutline {
                level: 1,
                title: "Beta Sub".to_string(),
                page_index: 0,
                y: points(666),
            },
        ];

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains(
            "5 0 obj\n<< /Type /Outlines /First 6 0 R /Last 8 0 R /Count 4 >>\nendobj\n"
        ));
        assert!(content.contains(
            "6 0 obj\n<< /Title (Alpha) /Parent 5 0 R /Next 8 0 R /First 7 0 R /Last 7 0 R /Count 1 /Dest [3 0 R /XYZ 72 720 0] >>\nendobj\n"
        ));
        assert!(content.contains(
            "7 0 obj\n<< /Title (Alpha Sub) /Parent 6 0 R /Dest [3 0 R /XYZ 72 702 0] >>\nendobj\n"
        ));
        assert!(content.contains(
            "8 0 obj\n<< /Title (Beta) /Parent 5 0 R /Prev 6 0 R /First 9 0 R /Last 9 0 R /Count 1 /Dest [3 0 R /XYZ 72 684 0] >>\nendobj\n"
        ));
        assert!(content.contains(
            "9 0 obj\n<< /Title (Beta Sub) /Parent 8 0 R /Dest [3 0 R /XYZ 72 666 0] >>\nendobj\n"
        ));
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
    fn unicode_to_winansi_maps_windows_1252_extras() {
        let cases = [
            ('\u{20AC}', 0x80),
            ('\u{201A}', 0x82),
            ('\u{0192}', 0x83),
            ('\u{201E}', 0x84),
            ('\u{2026}', 0x85),
            ('\u{2020}', 0x86),
            ('\u{2021}', 0x87),
            ('\u{02C6}', 0x88),
            ('\u{2030}', 0x89),
            ('\u{0160}', 0x8A),
            ('\u{2039}', 0x8B),
            ('\u{0152}', 0x8C),
            ('\u{017D}', 0x8E),
            ('\u{2018}', 0x91),
            ('\u{2019}', 0x92),
            ('\u{201C}', 0x93),
            ('\u{201D}', 0x94),
            ('\u{2022}', 0x95),
            ('\u{2013}', 0x96),
            ('\u{2014}', 0x97),
            ('\u{02DC}', 0x98),
            ('\u{2122}', 0x99),
            ('\u{0161}', 0x9A),
            ('\u{203A}', 0x9B),
            ('\u{0153}', 0x9C),
            ('\u{017E}', 0x9E),
            ('\u{0178}', 0x9F),
        ];

        for (ch, expected) in cases {
            assert_eq!(unicode_to_winansi(ch), Some(expected));
        }
        assert_eq!(unicode_to_winansi('A'), Some(b'A'));
        assert_eq!(unicode_to_winansi('\u{00B7}'), Some(0xB7));
        assert_eq!(unicode_to_winansi('δ'), None);
    }

    #[test]
    fn encode_pdf_text_ascii_passthrough() {
        let result = encode_pdf_text("Hello, Ferritex!");

        assert_eq!(result.encoded, "Hello, Ferritex!");
        assert!(result.unencodable_chars.is_empty());
    }

    #[test]
    fn encode_pdf_actual_text_ascii_uses_literal_string() {
        assert_eq!(
            encode_pdf_actual_text("Inline x2 y1 note3"),
            "(Inline x2 y1 note3)"
        );
    }

    #[test]
    fn encode_pdf_text_winansi_mappable() {
        let result = encode_pdf_text("• · —");

        assert_eq!(result.encoded, r"\225 \267 \227");
        assert!(result.unencodable_chars.is_empty());
    }

    #[test]
    fn encode_pdf_text_replaces_unencodable() {
        let result = encode_pdf_text("δ∫");

        assert_eq!(result.encoded, "??");
        assert_eq!(result.unencodable_chars, vec!['δ', '∫']);
    }

    #[test]
    fn encode_pdf_text_escapes_special() {
        let result = encode_pdf_text(r#"A (test) \ sample"#);

        assert_eq!(result.encoded, r#"A \(test\) \\ sample"#);
        assert!(result.unencodable_chars.is_empty());
    }

    #[test]
    fn render_non_ascii_winansi_in_pdf() {
        let pdf = PdfRenderer::default().render(&single_page(&["• ·"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains(r"(\225 \267) Tj"));
        assert!(!pdf
            .bytes
            .windows("•".len())
            .any(|window| window == "•".as_bytes()));
        assert!(!pdf
            .bytes
            .windows("·".len())
            .any(|window| window == "·".as_bytes()));
    }

    #[test]
    fn render_unencodable_chars_produces_errors() {
        // '漢' has no WinAnsi mapping and no Symbol-font mapping, so it still
        // triggers an explicit encoding error.
        let pdf = PdfRenderer::default().render(&single_page(&["漢 漢"]));

        assert_eq!(pdf.encoding_errors.len(), 1);
        let error = &pdf.encoding_errors[0];
        assert!(error.contains("is not supported"), "{error}");
        assert!(error.contains('漢'), "{error}");
        assert!(error.contains("U+6F22"), "{error}");
    }

    #[test]
    fn renders_script_lines_with_unicode_actual_text_as_utf16be_hex() {
        let scripted = format!(
            "α {}π{} ∫",
            crate::typesetting::math_layout::SUPERSCRIPT_START_MARKER,
            crate::typesetting::math_layout::SUPERSCRIPT_END_MARKER,
        );
        let mut document = single_page(&[]);
        document.pages[0].lines = vec![TextLine {
            text: scripted,
            x: DimensionValue::zero(),
            y: points(720),
            links: Vec::new(),
            font_index: 0,
            font_size: points(10),
            source_span: None,
        }];

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);
        let payloads = actual_text_payloads(&content);

        assert!(
            payloads.contains(&"<FEFF03B1002003C00020222B>"),
            "expected UTF-16BE ActualText payload, got: {payloads:?}"
        );
        assert!(
            payloads.iter().all(|payload| !payload.contains('?')),
            "ActualText payloads must not contain '?': {payloads:?}"
        );
    }

    #[test]
    fn math_mode_unicode_glyphs_use_symbol_font_and_dont_warn() {
        // Glyphs from Issue #9: α, β, γ, π, √, ∞, ∫ plus thin-space (U+2009).
        let pdf = PdfRenderer::default().render(&single_page(&[
            "\u{03B1} + \u{03B2} = \u{03B3} \u{03C0} \u{2009} \u{221A} \u{221E} \u{222B}",
        ]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        // No warning should be emitted for any of these math-mode glyphs.
        assert!(
            pdf.encoding_errors.is_empty(),
            "expected no PDF encoding errors, got: {:?}",
            pdf.encoding_errors,
        );

        // A dedicated Symbol font must be declared and referenced as /F2.
        assert!(content.contains("<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>"));
        assert!(content.contains("/F2 "));

        // Math-mode glyphs should be rendered through the Symbol font rather
        // than appearing as '?' placeholders in the content stream.
        assert!(content.contains("/F2 "));
        assert!(!content.matches("(?)").any(|_| true));
    }

    #[test]
    fn builtin_type1_font_has_winansi_encoding() {
        let pdf = PdfRenderer::default().render(&single_page(&["Hello, Ferritex!"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
        ));
    }

    #[test]
    fn renders_builtin_font_reference_by_default() {
        let pdf = PdfRenderer::default().render(&single_page(&["Hello, Ferritex!"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains(
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
        ));
        // Default resources carry Helvetica as F1 plus the always-appended
        // Symbol math font as F2.
        assert!(content.contains("/Resources << /Font << /F1 5 0 R /F2 6 0 R >> >>"));
        assert!(content.contains("<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>"));
    }

    #[test]
    fn resolves_named_latex_colors() {
        assert_eq!(resolve_named_color("red"), Some((1.0, 0.0, 0.0)));
        assert_eq!(resolve_named_color("blue"), Some((0.0, 0.0, 1.0)));
        assert_eq!(resolve_named_color("green"), Some((0.0, 1.0, 0.0)));
        assert_eq!(resolve_named_color("cyan"), Some((0.0, 1.0, 1.0)));
        assert_eq!(resolve_named_color("magenta"), Some((1.0, 0.0, 1.0)));
        assert_eq!(resolve_named_color("yellow"), Some((1.0, 1.0, 0.0)));
        assert_eq!(resolve_named_color("black"), Some((0.0, 0.0, 0.0)));
        assert_eq!(resolve_named_color("white"), Some((1.0, 1.0, 1.0)));
        assert_eq!(resolve_named_color("darkgray"), Some((0.25, 0.25, 0.25)));
        assert_eq!(resolve_named_color("darkgrey"), Some((0.25, 0.25, 0.25)));
        assert_eq!(resolve_named_color("gray"), Some((0.5, 0.5, 0.5)));
        assert_eq!(resolve_named_color("grey"), Some((0.5, 0.5, 0.5)));
        assert_eq!(resolve_named_color("lightgray"), Some((0.75, 0.75, 0.75)));
        assert_eq!(resolve_named_color("lightgrey"), Some((0.75, 0.75, 0.75)));
        assert_eq!(resolve_named_color("brown"), Some((0.75, 0.5, 0.25)));
        assert_eq!(resolve_named_color("olive"), Some((0.5, 0.5, 0.0)));
        assert_eq!(resolve_named_color("orange"), Some((1.0, 0.5, 0.0)));
        assert_eq!(resolve_named_color("pink"), Some((1.0, 0.75, 0.75)));
        assert_eq!(resolve_named_color("purple"), Some((0.75, 0.0, 0.25)));
        assert_eq!(resolve_named_color("teal"), Some((0.0, 0.5, 0.5)));
        assert_eq!(resolve_named_color("violet"), Some((0.5, 0.0, 0.5)));
        assert_eq!(resolve_named_color("unknown"), None);
    }

    #[test]
    fn colorlinks_render_colored_text_and_restore_default_color() {
        let mut document = single_page(&[]);
        document.pages[0].lines = vec![linked_line(
            "prefix link suffix",
            7,
            11,
            "https://example.com",
        )];
        document.navigation.default_link_style = LinkStyle {
            color_links: true,
            link_color: Some("red".to_string()),
        };

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("(prefix ) Tj\n1 0 0 rg\n(link) Tj\n0 0 0 rg\n( suffix) Tj"));
        assert!(content.contains("/Border [0 0 0]"));
    }

    #[test]
    fn default_link_annotations_have_visible_blue_border() {
        let mut document = single_page(&[]);
        document.pages[0].lines = vec![linked_line(
            "prefix link suffix",
            7,
            11,
            "https://example.com",
        )];

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/Border [0 0 1] /C [0 0 1]"));
        assert!(!content.contains("1 0 0 rg"));
    }

    #[test]
    fn internal_links_emit_goto_actions_and_named_destinations() {
        let mut document = single_page(&["see intro", "1 Intro"]);
        document.pages[0].lines[0].links = vec![TextLineLink {
            url: "#sec:intro".to_string(),
            start_char: 0,
            end_char: 9,
        }];
        document.named_destinations = vec![TypesetNamedDestination {
            name: "sec:intro".to_string(),
            page_index: 0,
            y: points(702),
        }];

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/S /GoTo /D (sec:intro)"));
        assert!(content.contains("/Names << /Dests"));
        assert!(content.contains("(sec:intro) [3 0 R /XYZ 72 702 0]"));
    }

    #[test]
    fn colorlinks_without_named_color_fall_back_to_blue_text() {
        let mut document = single_page(&[]);
        document.pages[0].lines = vec![linked_line(
            "prefix link suffix",
            7,
            11,
            "https://example.com",
        )];
        document.navigation.default_link_style = LinkStyle {
            color_links: true,
            link_color: Some("unknown".to_string()),
        };

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("0 0 1 rg\n(link) Tj\n0 0 0 rg"));
    }

    #[test]
    fn colorlinks_none_link_color_falls_back_to_blue() {
        let mut document = single_page(&[]);
        document.pages[0].lines = vec![linked_line(
            "prefix link suffix",
            7,
            11,
            "https://example.com",
        )];
        document.navigation.default_link_style = LinkStyle {
            color_links: true,
            link_color: None,
        };

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("0 0 1 rg\n(link) Tj\n0 0 0 rg"));
    }

    #[test]
    fn colorlinks_multiple_links_on_same_line() {
        let mut document = single_page(&[]);
        document.pages[0].lines = vec![TextLine {
            text: "aaa bbb ccc".to_string(),
            x: DimensionValue::zero(),
            y: points(720),
            links: vec![
                TextLineLink {
                    url: "https://a.com".to_string(),
                    start_char: 4,
                    end_char: 7,
                },
                TextLineLink {
                    url: "https://b.com".to_string(),
                    start_char: 8,
                    end_char: 11,
                },
            ],
            font_index: 0,
            font_size: points(10),
            source_span: None,
        }];
        document.navigation.default_link_style = LinkStyle {
            color_links: true,
            link_color: Some("red".to_string()),
        };

        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains(
            "(aaa ) Tj\n1 0 0 rg\n(bbb) Tj\n0 0 0 rg\n( ) Tj\n1 0 0 rg\n(ccc) Tj\n0 0 0 rg"
        ));
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
    fn renders_embedded_type1_font_objects() {
        let renderer = PdfRenderer::with_fonts(vec![embedded_type1_font_resource()]);
        let pdf = renderer.render(&single_page(&["Hello, Ferritex!"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/Subtype /Type1 /BaseFont /FERRTX+CMR10"));
        assert!(content.contains("/FontDescriptor 6 0 R"));
        assert!(content.contains("/FontFile 7 0 R"));
        assert!(content.contains("/Length1 3"));
        assert!(content.contains("/Length2 4"));
        assert!(content.contains("/Length3 2"));
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
    fn renders_type1_font_descriptor_with_configured_flags() {
        let renderer = PdfRenderer::with_fonts(vec![embedded_type1_font_resource()]);
        let pdf = renderer.render(&single_page(&["Hello, Ferritex!"]));
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("/Flags 6"));
        assert!(content.contains("/FontBBox [-251 -250 1009 750]"));
        assert!(content.contains("/Ascent 683"));
        assert!(content.contains("/Descent -217"));
        assert!(content.contains("/StemV 69"));
        assert!(content.contains("/CapHeight 683"));
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

        // F1 = Helvetica (1 object: 5), F2 = embedded TrueType (3 consecutive
        // objects starting at 6: dict/descriptor/fontfile), F3 = Symbol math
        // font (always appended at 9).
        assert!(content.contains("/Resources << /Font << /F1 5 0 R /F2 6 0 R /F3 9 0 R >> >>"));
        assert!(content.contains(
            "5 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
        ));
        assert!(content.contains("6 0 obj\n<< /Type /Font /Subtype /TrueType"));
        assert!(content.contains("9 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>"));
    }

    #[test]
    fn switches_font_resources_when_line_font_changes() {
        let mut document = single_page(&[]);
        document.pages[0].lines = vec![
            TextLine {
                text: "Main".to_string(),
                x: DimensionValue::zero(),
                y: points(720),
                links: Vec::new(),
                font_index: 0,
                font_size: points(10),
                source_span: None,
            },
            TextLine {
                text: "Sans".to_string(),
                x: DimensionValue::zero(),
                y: points(702),
                links: Vec::new(),
                font_index: 1,
                font_size: points(10),
                source_span: None,
            },
            TextLine {
                text: "Mono".to_string(),
                x: DimensionValue::zero(),
                y: points(684),
                links: Vec::new(),
                font_index: 2,
                font_size: points(10),
                source_span: None,
            },
        ];
        let renderer = PdfRenderer::with_fonts(vec![
            FontResource::BuiltinType1 {
                base_font: "Helvetica".to_string(),
            },
            FontResource::BuiltinType1 {
                base_font: "Times-Roman".to_string(),
            },
            FontResource::BuiltinType1 {
                base_font: "Courier".to_string(),
            },
        ]);

        let pdf = renderer.render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert!(content.contains("BT\n/F1 10 Tf\n72 720 Td\n(Main) Tj"));
        assert!(content.contains("0 -18 Td\n/F2 10 Tf\n(Sans) Tj"));
        assert!(content.contains("0 -18 Td\n/F3 10 Tf\n(Mono) Tj"));
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

    #[test]
    fn xref_entries_point_to_object_markers_with_mixed_indirect_objects() {
        // Regression for issue #23: PDFs containing images, form XObjects,
        // outlines, and named destinations were written with object IDs that
        // did not match the write order, leaving xref offsets pointing at the
        // wrong bytes. This exercises the full set and verifies the xref
        // table actually reaches each `{id} 0 obj` marker.
        let mut document = single_page_with_images(
            &["Body"],
            vec![
                TypesetImage {
                    scene: raster_scene(),
                    x: points(72),
                    y: points(600),
                    display_width: points(100),
                    display_height: points(100),
                },
                TypesetImage {
                    scene: pdf_scene(),
                    x: points(72),
                    y: points(480),
                    display_width: points(100),
                    display_height: points(50),
                },
            ],
        );
        document.outlines = vec![TypesetOutline {
            level: 0,
            title: "Heading".to_string(),
            page_index: 0,
            y: points(720),
        }];
        document.named_destinations = vec![TypesetNamedDestination {
            name: "heading".to_string(),
            page_index: 0,
            y: points(720),
        }];

        let renderer = PdfRenderer::default()
            .with_images(
                vec![PdfImageXObject {
                    object_id: 0,
                    width: 1,
                    height: 1,
                    color_space: ImageColorSpace::DeviceRGB,
                    bits_per_component: 8,
                    data: vec![120, 156, 99, 248, 207, 192, 0, 0, 3, 1, 1, 0],
                    filter: ImageFilter::FlateDecode,
                }],
                vec![vec![PlacedImage {
                    xobject_index: 0,
                    x: points(72),
                    y: points(600),
                    display_width: points(100),
                    display_height: points(100),
                }]],
            )
            .with_form_xobjects(
                vec![PdfFormXObject {
                    object_id: 0,
                    media_box: [0.0, 0.0, 200.0, 100.0],
                    data: b"0 0 m\n200 100 l\nS".to_vec(),
                    resources_dict: Some("<< /ProcSet [/PDF] >>".to_string()),
                }],
                vec![vec![PlacedFormXObject {
                    xobject_index: 0,
                    x: points(72),
                    y: points(480),
                    display_width: points(100),
                    display_height: points(50),
                }]],
            );

        let pdf = renderer.render(&document);
        let bytes = pdf.bytes.as_slice();

        // Locate the xref section header at the start of a line.
        let xref_header = b"\nxref\n";
        let xref_pos = bytes
            .windows(xref_header.len())
            .position(|window| window == xref_header)
            .expect("rendered PDF must contain an xref section")
            + 1;
        let xref_body = std::str::from_utf8(&bytes[xref_pos..])
            .expect("xref section should be ASCII");
        let mut lines = xref_body.lines();
        let header = lines.next().expect("xref header");
        assert_eq!(header, "xref");
        let subsection = lines.next().expect("xref subsection header");
        let mut parts = subsection.split_whitespace();
        let first_id: usize = parts
            .next()
            .and_then(|value| value.parse().ok())
            .expect("first object id");
        let count: usize = parts
            .next()
            .and_then(|value| value.parse().ok())
            .expect("object count");
        assert_eq!(first_id, 0, "xref subsection should start at object 0");
        assert!(count >= 2, "PDF must contain at least the catalog and pages");

        // The first entry is the free object (id 0); skip it. For every other
        // declared object id, verify the byte offset actually lands on the
        // `{id} 0 obj` marker.
        let free_entry = lines.next().expect("free entry line");
        assert!(
            free_entry.starts_with("0000000000"),
            "free entry must point at offset 0: {free_entry}"
        );
        for object_id in 1..count {
            let line = lines
                .next()
                .unwrap_or_else(|| panic!("missing xref entry for object {object_id}"));
            let offset: usize = line
                .split_whitespace()
                .next()
                .and_then(|value| value.parse().ok())
                .unwrap_or_else(|| panic!("unparsable xref entry: {line}"));
            let expected_marker = format!("{object_id} 0 obj");
            let actual = std::str::from_utf8(
                &bytes[offset..offset.saturating_add(expected_marker.len())],
            )
            .unwrap_or_else(|_| panic!("non-UTF8 bytes at offset {offset}"));
            assert_eq!(
                actual, expected_marker,
                "xref entry for object {object_id} at offset {offset} should start with {expected_marker:?}"
            );
        }
    }

    fn embedded_font_resource() -> FontResource {
        embedded_font_resource_with_data(vec![1, 2, 3, 4, 5])
    }

    fn embedded_type1_font_resource() -> FontResource {
        FontResource::EmbeddedType1 {
            base_font: "FERRTX+CMR10".to_string(),
            ascii_length: 3,
            binary_length: 4,
            trailer_length: 2,
            font_program: vec![1, 2, 3, 4, 5, 6, 7, 8, 9],
            first_char: 32,
            last_char: 34,
            widths: vec![250, 300, 325],
            bbox: [-251, -250, 1009, 750],
            ascent: 683,
            descent: -217,
            italic_angle: 0,
            stem_v: 69,
            cap_height: 683,
            flags: 6,
        }
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
