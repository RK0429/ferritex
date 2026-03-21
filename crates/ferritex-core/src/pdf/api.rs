pub use crate::graphics::api::ImageColorSpace;
use crate::kernel::api::DimensionValue;
use crate::typesetting::api::{TextLine, TypesetDocument, TypesetOutline, TypesetPage};

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacedImage {
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
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PdfLinkAnnotation {
    object_id: usize,
    url: String,
    x_start: DimensionValue,
    x_end: DimensionValue,
    y_bottom: DimensionValue,
    y_top: DimensionValue,
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

    pub fn render(&self, document: &TypesetDocument) -> PdfDocument {
        let page_count = document.pages.len();
        let total_lines = document.pages.iter().map(|page| page.lines.len()).sum();
        let mut pdf = Vec::<u8>::new();
        let mut offsets = Vec::<usize>::new();
        let page_object_start = 3usize;
        let content_object_start = page_object_start + page_count;
        let annotation_object_start = content_object_start + page_count;
        let mut image_objects = self.images.clone();
        let mut page_annotations = document
            .pages
            .iter()
            .map(page_link_annotations)
            .collect::<Vec<_>>();
        let next_object_after_annotations =
            assign_annotation_object_ids(&mut page_annotations, annotation_object_start);
        let outline_root_object_id =
            (!document.outlines.is_empty()).then_some(next_object_after_annotations);
        let outline_item_object_start =
            next_object_after_annotations + usize::from(outline_root_object_id.is_some());
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
        let info_object_id =
            build_info_dictionary(document).map(|_| image_object_start + image_objects.len());
        let page_font_resources = page_font_resources(&font_objects);
        let catalog_outlines = outline_root_object_id
            .map(|object_id| format!(" /Outlines {object_id} 0 R"))
            .unwrap_or_default();

        pdf.extend_from_slice(b"%PDF-1.4\n");
        append_object(
            &mut pdf,
            &mut offsets,
            &format!("1 0 obj\n<< /Type /Catalog /Pages 2 0 R{catalog_outlines} >>\nendobj\n"),
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
            let annots_entry = if page_annots.is_empty() {
                String::new()
            } else {
                format!(" /Annots [{}]", page_annots.join(" "))
            };
            let xobject_resources = page_xobject_resources(page_images, &image_objects);
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

        for (page_index, page) in document.pages.iter().enumerate() {
            let content_object_id = content_object_start + page_index;
            let page_images = self
                .page_images
                .get(page_index)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let stream = render_page_stream(page, page_images, &image_objects);
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

        for annotations in &page_annotations {
            for annotation in annotations {
                append_object(
                    &mut pdf,
                    &mut offsets,
                    &format!(
                        "{} 0 obj\n<< /Type /Annot /Subtype /Link /Rect [{} {} {} {}] /Border [0 0 0] /A << /Type /Action /S /URI /URI ({}) >> >>\nendobj\n",
                        annotation.object_id,
                        points_to_pdf_number(annotation.x_start),
                        points_to_pdf_number(annotation.y_bottom),
                        points_to_pdf_number(annotation.x_end),
                        points_to_pdf_number(annotation.y_top),
                        escape_pdf_text(&annotation.url),
                    ),
                );
            }
        }

        for image in &image_objects {
            append_image_xobject(&mut pdf, &mut offsets, image);
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

fn render_page_stream(
    page: &TypesetPage,
    placed_images: &[PlacedImage],
    image_objects: &[PdfImageXObject],
) -> String {
    let mut stream = String::new();

    if let Some(first_line) = page.lines.first() {
        stream.push_str("BT\n/F1 12 Tf\n");
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
        stream.push_str("ET\n");
    }

    for placement in placed_images {
        if image_objects.get(placement.xobject_index).is_some() {
            stream.push_str(&render_image_placement(placement));
        }
    }

    stream
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

fn assign_image_object_ids(images: &mut [PdfImageXObject], start_object_id: usize) {
    for (index, image) in images.iter_mut().enumerate() {
        image.object_id = start_object_id + index;
    }
}

fn page_xobject_resources(
    page_images: &[PlacedImage],
    image_objects: &[PdfImageXObject],
) -> String {
    let resources = page_images
        .iter()
        .filter_map(|placement| {
            image_objects
                .get(placement.xobject_index)
                .map(|image| format!("/Im{} {} 0 R", placement.xobject_index + 1, image.object_id))
        })
        .collect::<std::collections::BTreeSet<_>>();

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
    page.lines.iter().flat_map(line_link_annotations).collect()
}

fn line_link_annotations(line: &TextLine) -> Vec<PdfLinkAnnotation> {
    line.links
        .iter()
        .filter(|link| !link.url.is_empty() && link.start_char < link.end_char)
        .map(|link| PdfLinkAnnotation {
            object_id: 0,
            url: link.url.clone(),
            x_start: points(LEFT_MARGIN_PT + LINK_CHAR_WIDTH_PT * link.start_char as i64),
            x_end: points(LEFT_MARGIN_PT + LINK_CHAR_WIDTH_PT * link.end_char as i64),
            y_bottom: line.y - points(LINK_DESCENT_PT),
            y_top: line.y + points(LINK_HEIGHT_PT - LINK_DESCENT_PT),
        })
        .collect()
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

fn build_info_dictionary(document: &TypesetDocument) -> Option<String> {
    let mut fields = Vec::new();

    if let Some(title) = document.title.as_deref().filter(|title| !title.is_empty()) {
        fields.push(format!("/Title ({})", escape_pdf_text(title)));
    }
    if let Some(author) = document
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
        FontResource, ImageColorSpace, ImageFilter, PdfImageXObject, PdfRenderer, PlacedImage,
    };
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
                    links: Vec::new(),
                })
                .collect(),
            images: Vec::new(),
        }
    }

    fn single_page(lines: &[&str]) -> TypesetDocument {
        TypesetDocument {
            pages: vec![page(lines)],
            outlines: Vec::new(),
            title: None,
            author: None,
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
            outlines: Vec::new(),
            title: None,
            author: None,
        };
        let pdf = PdfRenderer::default().render(&document);
        let content = String::from_utf8_lossy(&pdf.bytes);

        assert_eq!(pdf.page_count, 2);
        assert!(content.contains("/Count 2"));
    }

    #[test]
    fn emits_image_xobject_and_placement_commands() {
        let document = single_page(&[]);
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
