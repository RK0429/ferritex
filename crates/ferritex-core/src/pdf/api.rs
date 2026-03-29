use std::thread;

use crate::compilation::{CommitBarrier, DocumentPartitionPlan, LinkStyle, StageCommitPayload};
use crate::graphics::api::{Color, GraphicNode, GraphicsScene, ImageColorSpace, PathSegment};
use crate::kernel::api::DimensionValue;
use crate::typesetting::api::{
    FloatPlacement, TextLine, TypesetDocument, TypesetOutline, TypesetPage,
};

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const LEFT_MARGIN_PT: i64 = 72;
const LINK_CHAR_WIDTH_PT: i64 = 6;
const LINK_HEIGHT_PT: i64 = 12;
const LINK_DESCENT_PT: i64 = 2;

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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PdfLinkAnnotation {
    object_id: usize,
    target: PdfLinkTarget,
    x_start: DimensionValue,
    x_end: DimensionValue,
    y_bottom: DimensionValue,
    y_top: DimensionValue,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PdfLinkTarget {
    Uri(String),
    InternalDestination(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutlineObject {
    object_id: usize,
    body: String,
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
        self.render_with_partition_plan(document, parallelism, 1, &DocumentPartitionPlan::default())
    }

    pub fn render_with_partition_plan(
        &self,
        document: &TypesetDocument,
        parallelism: usize,
        pass_number: u32,
        partition_plan: &DocumentPartitionPlan,
    ) -> PdfDocument {
        let page_count = document.pages.len();
        let total_lines = document.pages.iter().map(|page| page.lines.len()).sum();
        let link_style = &document.navigation.default_link_style;
        let mut pdf = Vec::<u8>::new();
        let mut offsets = Vec::<usize>::new();
        let page_object_start = 3usize;
        let content_object_start = page_object_start + page_count;
        let annotation_object_start = content_object_start + page_count;
        let mut image_objects = self.images.clone();
        let mut form_xobjects = self.form_xobjects.clone();
        let page_partition_ids = page_partition_ids_for_plan(document, partition_plan);
        let page_payloads = render_page_payloads(
            &document.pages,
            &self.page_images,
            &image_objects,
            &self.page_form_xobjects,
            &form_xobjects,
            link_style,
            &page_partition_ids,
            parallelism,
            pass_number,
        );
        let mut page_annotations = page_payloads
            .iter()
            .map(|payload| payload.annotations.clone())
            .collect::<Vec<_>>();
        let next_object_after_annotations =
            assign_annotation_object_ids(&mut page_annotations, annotation_object_start);
        let named_destination_object_id =
            (!document.named_destinations.is_empty()).then_some(next_object_after_annotations);
        let outline_root_object_id = (!document.outlines.is_empty()).then_some(
            next_object_after_annotations + usize::from(named_destination_object_id.is_some()),
        );
        let outline_item_object_start = next_object_after_annotations
            + usize::from(named_destination_object_id.is_some())
            + usize::from(outline_root_object_id.is_some());
        let outline_objects = outline_root_object_id.map_or_else(Vec::new, |root_object_id| {
            build_outline_objects(
                &document.outlines,
                page_object_start,
                root_object_id,
                outline_item_object_start,
            )
        });
        let font_object_start = outline_item_object_start + outline_objects.len();
        let font_objects = build_font_objects(&self.fonts, font_object_start);
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
            &format!(
                "1 0 obj\n<< /Type /Catalog /Pages 2 0 R{catalog_named_destinations}{catalog_outlines} >>\nendobj\n"
            ),
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
            append_object(
                &mut pdf,
                &mut offsets,
                &format!(
                    "{page_object_id} 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {} {}] /Contents {content_object_id} 0 R /Resources << /Font << {} >>{} >>{annots_entry} >>\nendobj\n",
                    points_to_pdf_number(page.page_box.width),
                    points_to_pdf_number(page.page_box.height),
                    page_font_resources,
                    xobject_resources,
                ),
            );
        }

        for (page_index, payload) in page_payloads.iter().enumerate() {
            let content_object_id = content_object_start + page_index;
            append_object(
                &mut pdf,
                &mut offsets,
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
                        escape_pdf_text(url)
                    ),
                    PdfLinkTarget::InternalDestination(name) => format!(
                        "/A << /Type /Action /S /GoTo /D ({}) >>",
                        escape_pdf_text(name)
                    ),
                };
                append_object(
                    &mut pdf,
                    &mut offsets,
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
                &build_named_destination_object(document, page_object_start, object_id),
            );
        }

        if let Some(root_object_id) = outline_root_object_id {
            let first_object_id = outline_objects.first().map(|item| item.object_id);
            let last_object_id = outline_objects.last().map(|item| item.object_id);
            append_object(
                &mut pdf,
                &mut offsets,
                &format!(
                    "{root_object_id} 0 obj\n<< /Type /Outlines{}{} /Count {} >>\nendobj\n",
                    first_object_id
                        .map(|object_id| format!(" /First {object_id} 0 R"))
                        .unwrap_or_default(),
                    last_object_id
                        .map(|object_id| format!(" /Last {object_id} 0 R"))
                        .unwrap_or_default(),
                    outline_objects.len(),
                ),
            );

            for outline_object in &outline_objects {
                append_object(
                    &mut pdf,
                    &mut offsets,
                    &format!(
                        "{} 0 obj\n{}\nendobj\n",
                        outline_object.object_id, outline_object.body
                    ),
                );
            }
        }

        for font_object in &font_objects {
            for object in &font_object.objects {
                append_object_bytes(&mut pdf, &mut offsets, object);
            }
        }

        if let (Some(object_id), Some(info_dictionary)) =
            (info_object_id, build_info_dictionary(document))
        {
            append_object(
                &mut pdf,
                &mut offsets,
                &format!("{object_id} 0 obj\n{info_dictionary}\nendobj\n"),
            );
        }

        let xref_offset = pdf.len();
        pdf.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
        pdf.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            pdf.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        pdf.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R{} >>\nstartxref\n{}\n%%EOF\n",
                offsets.len() + 1,
                info_object_id
                    .map(|object_id| format!(" /Info {object_id} 0 R"))
                    .unwrap_or_default(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct PageRenderPayload {
    page_index: usize,
    annotations: Vec<PdfLinkAnnotation>,
    stream: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PartitionRenderPayload {
    page_payloads: Vec<PageRenderPayload>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PageRenderWorkload {
    partition_id: String,
    page_indices: Vec<usize>,
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
    pass_number: u32,
) -> Vec<PageRenderPayload> {
    let workloads = page_render_workloads(page_partition_ids, pages.len(), parallelism);
    if workloads.len() <= 1 {
        return workloads
            .into_iter()
            .flat_map(|workload| {
                workload
                    .page_indices
                    .into_iter()
                    .map(|page_index| PageRenderPayload {
                        page_index,
                        annotations: page_link_annotations(&pages[page_index]),
                        stream: render_page_stream(
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
                        ),
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
    }

    let payloads = thread::scope(|scope| {
        let mut handles = Vec::new();
        for workload in workloads {
            handles.push(scope.spawn(move || {
                let mut page_payloads = workload
                    .page_indices
                    .iter()
                    .copied()
                    .map(|page_index| {
                        let page = &pages[page_index];
                        PageRenderPayload {
                            page_index,
                            annotations: page_link_annotations(page),
                            stream: render_page_stream(
                                page,
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
                            ),
                        }
                    })
                    .collect::<Vec<_>>();
                page_payloads.sort_by_key(|payload| payload.page_index);
                StageCommitPayload::layout_merge(
                    workload.partition_id,
                    PartitionRenderPayload { page_payloads },
                )
            }));
        }

        handles
            .into_iter()
            .rev()
            .map(|handle| handle.join().expect("page render worker should not panic"))
            .collect::<Vec<_>>()
    });
    let mut barrier = CommitBarrier::new(pass_number);
    for payload in payloads {
        barrier.commit(payload);
    }
    barrier
        .into_ordered()
        .into_iter()
        .flat_map(|payload| payload.payload.page_payloads)
        .collect()
}

fn page_partition_ids_for_plan(
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
) -> String {
    let mut stream = String::new();

    stream.push_str(&render_text_lines(&page.lines, link_style));
    for placement in &page.float_placements {
        let lines = resolve_float_lines(placement);
        stream.push_str(&render_text_lines(&lines, link_style));
    }

    let mut image_index = 0usize;
    let mut form_index = 0usize;
    for image in &page.images {
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

    stream
}

fn render_text_lines(lines: &[TextLine], link_style: &LinkStyle) -> String {
    let Some(first_line) = lines.first() else {
        return String::new();
    };

    let mut stream = format!("BT\n/F{} 12 Tf\n", first_line.font_index + 1);
    let mut current_font = first_line.font_index;
    stream.push_str(&format!(
        "{} {} Td\n",
        LEFT_MARGIN_PT,
        points_to_pdf_number(first_line.y)
    ));
    render_text_line(&mut stream, first_line, link_style);

    let mut previous_y = first_line.y;
    for line in &lines[1..] {
        stream.push_str(&format!(
            "0 {} Td\n",
            points_to_pdf_number(line.y - previous_y)
        ));
        if line.font_index != current_font {
            stream.push_str(&format!("/F{} 12 Tf\n", line.font_index + 1));
            current_font = line.font_index;
        }
        render_text_line(&mut stream, line, link_style);
        previous_y = line.y;
    }
    stream.push_str("ET\n");
    stream
}

fn render_text_line(stream: &mut String, line: &TextLine, link_style: &LinkStyle) {
    let Some(link_color) = active_link_color(link_style) else {
        stream.push_str(&format!("({}) Tj\n", escape_pdf_text(&line.text)));
        return;
    };

    let mut links = line
        .links
        .iter()
        .filter(|link| !link.url.is_empty() && link.start_char < link.end_char)
        .collect::<Vec<_>>();
    if links.is_empty() {
        stream.push_str(&format!("({}) Tj\n", escape_pdf_text(&line.text)));
        return;
    }

    links.sort_by_key(|link| (link.start_char, link.end_char));
    let boundaries = char_boundaries(&line.text);
    let char_count = boundaries.len().saturating_sub(1);
    let mut cursor = 0usize;
    for link in links {
        let start = link.start_char.min(char_count);
        let end = link.end_char.min(char_count);
        if start >= end || start < cursor {
            continue;
        }
        if cursor < start {
            stream.push_str(&format!(
                "({}) Tj\n",
                escape_pdf_text(char_slice(&line.text, &boundaries, cursor, start))
            ));
        }
        stream.push_str(&pdf_rgb_operator(link_color));
        stream.push_str(&format!(
            "({}) Tj\n",
            escape_pdf_text(char_slice(&line.text, &boundaries, start, end))
        ));
        stream.push_str("0 0 0 rg\n");
        cursor = end;
    }

    if cursor < char_count {
        stream.push_str(&format!(
            "({}) Tj\n",
            escape_pdf_text(char_slice(&line.text, &boundaries, cursor, char_count))
        ));
    }
}

fn resolve_float_lines(placement: &FloatPlacement) -> Vec<TextLine> {
    placement
        .content
        .lines
        .iter()
        .map(|line| TextLine {
            text: line.text.clone(),
            y: placement.y_position - line.y,
            links: line.links.clone(),
            font_index: line.font_index,
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
        match node {
            GraphicNode::Vector(primitive) => body.push_str(&render_vector_primitive(primitive)),
            GraphicNode::Text(text) => body.push_str(&render_graphic_text(text)),
            GraphicNode::External(_) | GraphicNode::Pdf(_) => {}
        }
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

fn render_vector_primitive(primitive: &crate::graphics::api::VectorPrimitive) -> String {
    if primitive.path.is_empty() {
        return String::new();
    }

    let mut stream = String::new();
    stream.push_str(&format!("{} w\n", pdf_real(primitive.line_width)));
    if let Some(stroke) = primitive.stroke {
        stream.push_str(&pdf_stroke_rgb_operator(color_components(stroke)));
    }
    if let Some(fill) = primitive.fill {
        stream.push_str(&pdf_rgb_operator(color_components(fill)));
    }

    for segment in &primitive.path {
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

    stream.push_str(
        match (primitive.stroke.is_some(), primitive.fill.is_some()) {
            (true, true) => "B\n",
            (true, false) => "S\n",
            (false, true) => "f\n",
            (false, false) => "n\n",
        },
    );
    stream
}

fn render_graphic_text(text: &crate::graphics::api::GraphicText) -> String {
    format!(
        "BT\n/F1 12 Tf\n0 0 0 rg\n{} {} Td\n({}) Tj\nET\n",
        pdf_real(text.position.x),
        pdf_real(text.position.y),
        escape_pdf_text(&text.content)
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

fn append_image_xobject(buffer: &mut Vec<u8>, offsets: &mut Vec<usize>, image: &PdfImageXObject) {
    offsets.push(buffer.len());
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
    offsets: &mut Vec<usize>,
    form_xobject: &PdfFormXObject,
) {
    offsets.push(buffer.len());
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
) -> Vec<OutlineObject> {
    outlines
        .iter()
        .enumerate()
        .map(|(index, outline)| {
            let object_id = first_object_id + index;
            let prev = (index > 0).then_some(object_id - 1);
            let next = (index + 1 < outlines.len()).then_some(object_id + 1);
            let page_object_id = page_object_start + outline.page_index;
            OutlineObject {
                object_id,
                body: format!(
                    "<< /Title ({}) /Parent {} 0 R{}{} /Dest [{} 0 R /XYZ {} {} 0] >>",
                    escape_pdf_text(&outline.title),
                    root_object_id,
                    prev.map(|id| format!(" /Prev {id} 0 R"))
                        .unwrap_or_default(),
                    next.map(|id| format!(" /Next {id} 0 R"))
                        .unwrap_or_default(),
                    page_object_id,
                    LEFT_MARGIN_PT,
                    points_to_pdf_number(outline.y),
                ),
            }
        })
        .collect()
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
                escape_pdf_text(&destination.name),
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
        fields.push(format!("/Title ({})", escape_pdf_text(title)));
    }
    if let Some(author) = document
        .navigation
        .metadata
        .author
        .as_deref()
        .filter(|author| !author.is_empty())
    {
        fields.push(format!("/Author ({})", escape_pdf_text(author)));
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
    use super::{
        resolve_named_color, FontResource, ImageColorSpace, ImageFilter, PdfFormXObject,
        PdfImageXObject, PdfRenderer, PlacedFormXObject, PlacedImage,
    };
    use crate::assets::api::{AssetHandle, LogicalAssetId};
    use crate::compilation::{
        DocumentPartitionPlan, DocumentWorkUnit, LinkStyle, PartitionKind, PartitionLocator,
    };
    use crate::graphics::api::{
        Color, ExternalGraphic, GraphicNode, GraphicText, GraphicsScene, ImageMetadata,
        PathSegment, PdfGraphic, PdfGraphicMetadata, Point, VectorPrimitive,
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
                    y: points(720 - (index as i64 * 18)),
                    links: Vec::new(),
                    font_index: 0,
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
            y: points(720),
            links: vec![TextLineLink {
                url: url.to_string(),
                start_char,
                end_char,
            }],
            font_index: 0,
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
                    y: points(0),
                    links: Vec::new(),
                    font_index: 0,
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
        assert!(content.contains("B"));
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
        let parallel = renderer.render_with_partition_plan(&document, 4, 2, &plan);

        assert_eq!(parallel.page_count, sequential.page_count);
        assert_eq!(parallel.total_lines, sequential.total_lines);
        assert_eq!(parallel.bytes, sequential.bytes);
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
    fn switches_font_resources_when_line_font_changes() {
        let mut document = single_page(&[]);
        document.pages[0].lines = vec![
            TextLine {
                text: "Main".to_string(),
                y: points(720),
                links: Vec::new(),
                font_index: 0,
                source_span: None,
            },
            TextLine {
                text: "Sans".to_string(),
                y: points(702),
                links: Vec::new(),
                font_index: 1,
                source_span: None,
            },
            TextLine {
                text: "Mono".to_string(),
                y: points(684),
                links: Vec::new(),
                font_index: 2,
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

        assert!(content.contains("BT\n/F1 12 Tf\n72 720 Td\n(Main) Tj"));
        assert!(content.contains("0 -18 Td\n/F2 12 Tf\n(Sans) Tj"));
        assert!(content.contains("0 -18 Td\n/F3 12 Tf\n(Mono) Tj"));
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
