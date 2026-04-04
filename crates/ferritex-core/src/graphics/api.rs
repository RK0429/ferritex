use serde::{Deserialize, Serialize};

use crate::assets::api::AssetHandle;
use crate::kernel::api::DimensionValue;
use crate::parser::api::IncludeGraphicsOptions;

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ImageColorSpace {
    DeviceRGB,
    DeviceGray,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageMetadata {
    pub width: u32,
    pub height: u32,
    pub color_space: ImageColorSpace,
    pub bits_per_component: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExternalGraphic {
    pub path: String,
    pub asset_handle: AssetHandle,
    pub metadata: ImageMetadata,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfGraphicMetadata {
    pub media_box: [f64; 4],
    pub page_data: Vec<u8>,
    pub resources_dict: Option<String>,
}

impl Eq for PdfGraphicMetadata {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PdfGraphic {
    pub path: String,
    pub asset_handle: AssetHandle,
    pub metadata: PdfGraphicMetadata,
}

impl Eq for PdfGraphic {}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

impl Eq for Point {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PathSegment {
    MoveTo(Point),
    LineTo(Point),
    CurveTo {
        control1: Point,
        control2: Point,
        end: Point,
    },
    ClosePath,
}

impl Eq for PathSegment {}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Color {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Eq for Color {}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Transform2D {
    pub x_shift: f64,
    pub y_shift: f64,
    pub scale: f64,
    pub rotate: f64,
}

impl Transform2D {
    pub fn compose(self, child: Self) -> Self {
        Self {
            x_shift: self.x_shift + child.x_shift,
            y_shift: self.y_shift + child.y_shift,
            scale: self.scale * child.scale,
            rotate: self.rotate + child.rotate,
        }
    }
}

impl Default for Transform2D {
    fn default() -> Self {
        Self {
            x_shift: 0.0,
            y_shift: 0.0,
            scale: 1.0,
            rotate: 0.0,
        }
    }
}

impl Eq for Transform2D {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum ArrowSpec {
    #[default]
    None,
    Forward,
    Backward,
    Both,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize, Default)]
pub enum DashPattern {
    #[default]
    /// Solid stroke.
    Solid,
    /// TikZ `dashed`: on 3pt, off 3pt.
    Dashed,
    /// TikZ `dotted`: uses `\pgflinewidth` for dot length; approximated as fixed 1pt here.
    Dotted,
    /// TikZ `densely dashed`: on 3pt, off 2pt.
    DenselyDashed,
    /// TikZ `densely dotted`: approximated as fixed 1pt dots with 1pt gaps.
    DenselyDotted,
    /// TikZ `loosely dashed`: on 3pt, off 6pt.
    LooselyDashed,
    /// TikZ `loosely dotted`: approximated as fixed 1pt dots with 4pt gaps.
    LooselyDotted,
    /// TikZ `dash dot`: on 3pt, off 2pt, on 1pt, off 2pt.
    DashDot,
    /// TikZ `dash dot dot`: on 3pt, off 2pt, on 1pt, off 2pt, on 1pt, off 2pt.
    DashDotDot,
}

impl Eq for DashPattern {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LineCap {
    #[default]
    Butt,
    Round,
    Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

fn default_opacity() -> f64 {
    1.0
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorPrimitive {
    pub path: Vec<PathSegment>,
    pub stroke: Option<Color>,
    pub fill: Option<Color>,
    pub line_width: f64,
    #[serde(default)]
    pub arrows: ArrowSpec,
    #[serde(default)]
    pub dash_pattern: DashPattern,
    #[serde(default)]
    pub line_cap: LineCap,
    #[serde(default)]
    pub line_join: LineJoin,
    #[serde(default = "default_opacity")]
    pub opacity: f64,
    #[serde(default = "default_opacity")]
    pub fill_opacity: f64,
}

impl Eq for VectorPrimitive {}

impl Default for VectorPrimitive {
    fn default() -> Self {
        Self {
            path: Vec::new(),
            stroke: None,
            fill: None,
            line_width: 0.0,
            arrows: ArrowSpec::default(),
            dash_pattern: DashPattern::default(),
            line_cap: LineCap::default(),
            line_join: LineJoin::default(),
            opacity: default_opacity(),
            fill_opacity: default_opacity(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphicGroup {
    pub children: Vec<GraphicNode>,
    pub default_stroke: Option<Color>,
    pub default_fill: Option<Color>,
    pub default_line_width: Option<f64>,
    pub clip_path: Option<Vec<PathSegment>>,
    #[serde(default)]
    pub transform: Transform2D,
}

impl Eq for GraphicGroup {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphicText {
    pub position: Point,
    pub content: String,
}

impl Eq for GraphicText {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GraphicNode {
    External(ExternalGraphic),
    Pdf(PdfGraphic),
    Group(GraphicGroup),
    Vector(VectorPrimitive),
    Text(GraphicText),
}

impl Eq for GraphicNode {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedGraphic {
    Raster(ExternalGraphic),
    Pdf(PdfGraphic),
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct GraphicsScene {
    pub nodes: Vec<GraphicNode>,
}

impl Eq for GraphicsScene {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GraphicsBox {
    pub width: DimensionValue,
    pub height: DimensionValue,
    pub scene: Option<GraphicsScene>,
}

impl Eq for GraphicsBox {}

pub trait GraphicAssetResolver {
    fn resolve(&self, path: &str) -> Option<ResolvedGraphic>;
}

pub fn compile_includegraphics(
    path: &str,
    options: &IncludeGraphicsOptions,
    resolver: &dyn GraphicAssetResolver,
) -> Option<GraphicsBox> {
    let graphic = resolver.resolve(path)?;
    let (node, natural_width, natural_height) = graphic_layout(graphic)?;
    let aspect_ratio = natural_height.0 as f64 / natural_width.0 as f64;

    let (mut width, mut height) = match (options.width, options.height) {
        (Some(width), Some(height)) => (width, height),
        (Some(width), None) => (width, scale_dimension(width, aspect_ratio)),
        (None, Some(height)) => (scale_dimension(height, 1.0 / aspect_ratio), height),
        (None, None) => (natural_width, natural_height),
    };

    if let Some(scale) = options.scale {
        if scale <= 0.0 {
            return None;
        }
        width = scale_dimension(width, scale);
        height = scale_dimension(height, scale);
    }

    Some(GraphicsBox {
        width,
        height,
        scene: Some(GraphicsScene { nodes: vec![node] }),
    })
}

pub fn compile_graphics_scene(scene: GraphicsScene) -> GraphicsBox {
    let Some((normalized_scene, width, height)) = normalize_graphics_scene(scene) else {
        return GraphicsBox {
            width: DimensionValue::zero(),
            height: DimensionValue::zero(),
            scene: Some(GraphicsScene::default()),
        };
    };

    GraphicsBox {
        width,
        height,
        scene: Some(normalized_scene),
    }
}

pub fn parse_image_metadata(data: &[u8]) -> Option<ImageMetadata> {
    if has_png_signature(data) {
        parse_png_metadata(data)
    } else if data.starts_with(&[0xFF, 0xD8]) {
        parse_jpeg_metadata(data)
    } else {
        None
    }
}

pub fn is_pdf_signature(data: &[u8]) -> bool {
    data.starts_with(b"%PDF-")
}

pub fn parse_pdf_metadata(data: &[u8]) -> Option<PdfGraphicMetadata> {
    if !is_pdf_signature(data) {
        return None;
    }

    let page_object = find_first_page_object(data)?;
    let media_box = extract_media_box(page_object)?;
    let page_data = extract_page_content_stream(data, extract_contents_reference(page_object)?)?;
    if page_data.is_empty() {
        return None;
    }

    Some(PdfGraphicMetadata {
        media_box,
        page_data,
        resources_dict: extract_resources_dict(data, page_object),
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PdfIndirectRef {
    object_number: u32,
    generation: u16,
}

fn find_first_page_object(data: &[u8]) -> Option<&[u8]> {
    let mut offset = 0usize;
    while let Some((_, object_body, next_offset)) = next_pdf_object(data, offset) {
        if object_contains_page_type(object_body) && find_bytes(object_body, b"/Contents").is_some()
        {
            return Some(object_body);
        }
        offset = next_offset;
    }
    None
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

        let Some(object_number) = std::str::from_utf8(&data[index..object_number_end])
            .ok()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            index += 1;
            continue;
        };
        let Some(generation) = std::str::from_utf8(&data[generation_start..generation_end])
            .ok()
            .and_then(|value| value.parse::<u16>().ok())
        else {
            index += 1;
            continue;
        };

        let body_start = obj_start + 3;
        let Some(endobj_offset) = find_bytes(&data[body_start..], b"endobj") else {
            index += 1;
            continue;
        };
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
    let mut offset = 0usize;
    while let Some(found) = find_bytes(&object_body[offset..], b"/Type /Page") {
        let boundary = offset + found + b"/Type /Page".len();
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

fn extract_media_box(object_body: &[u8]) -> Option<[f64; 4]> {
    let media_box_start = find_bytes(object_body, b"/MediaBox")?;
    let array_start = skip_pdf_whitespace(object_body, media_box_start + b"/MediaBox".len());
    if object_body.get(array_start) != Some(&b'[') {
        return None;
    }

    let array_end = object_body[array_start..]
        .iter()
        .position(|byte| *byte == b']')
        .map(|offset| array_start + offset)?;
    let array = std::str::from_utf8(object_body.get(array_start + 1..array_end)?).ok()?;
    let values = array
        .split_ascii_whitespace()
        .map(str::parse::<f64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    if values.len() != 4 {
        return None;
    }

    Some([values[0], values[1], values[2], values[3]])
}

fn extract_contents_reference(object_body: &[u8]) -> Option<PdfIndirectRef> {
    let contents_start = find_bytes(object_body, b"/Contents")?;
    let contents_start = skip_pdf_whitespace(object_body, contents_start + b"/Contents".len());

    if object_body.get(contents_start) == Some(&b'[') {
        let array_end = object_body[contents_start..]
            .iter()
            .position(|byte| *byte == b']')
            .map(|offset| contents_start + offset)?;
        let ref_start = object_body[contents_start + 1..array_end]
            .iter()
            .position(|byte| byte.is_ascii_digit())
            .map(|offset| contents_start + 1 + offset)?;
        return parse_indirect_ref(&object_body[ref_start..array_end]);
    }

    parse_indirect_ref(&object_body[contents_start..])
}

fn parse_indirect_ref(data: &[u8]) -> Option<PdfIndirectRef> {
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

    Some(PdfIndirectRef {
        object_number: std::str::from_utf8(&data[object_number_start..object_number_end])
            .ok()?
            .parse()
            .ok()?,
        generation: std::str::from_utf8(&data[generation_start..generation_end])
            .ok()?
            .parse()
            .ok()?,
    })
}

fn extract_page_content_stream(data: &[u8], contents_ref: PdfIndirectRef) -> Option<Vec<u8>> {
    let object_body = find_object_by_ref(data, contents_ref)?;
    let stream_start = find_bytes(object_body, b"stream")?;
    if find_bytes(&object_body[..stream_start], b"/Filter").is_some() {
        return None;
    }

    let mut content_start = stream_start + b"stream".len();
    if object_body.get(content_start..content_start + 2) == Some(b"\r\n") {
        content_start += 2;
    } else if object_body.get(content_start) == Some(&b'\n')
        || object_body.get(content_start) == Some(&b'\r')
    {
        content_start += 1;
    } else {
        return None;
    }

    let endstream_offset = find_bytes(&object_body[content_start..], b"endstream")?;
    let mut content_end = content_start + endstream_offset;
    if content_end >= content_start + 2
        && object_body.get(content_end - 2..content_end) == Some(b"\r\n")
    {
        content_end -= 2;
    } else if content_end > content_start
        && (object_body.get(content_end - 1) == Some(&b'\n')
            || object_body.get(content_end - 1) == Some(&b'\r'))
    {
        content_end -= 1;
    }

    Some(object_body.get(content_start..content_end)?.to_vec())
}

fn extract_resources_dict(data: &[u8], page_object: &[u8]) -> Option<String> {
    let resources_start = find_bytes(page_object, b"/Resources")?;
    let resources_start = skip_pdf_whitespace(page_object, resources_start + b"/Resources".len());

    let dict_bytes = if page_object.get(resources_start..resources_start + 2) == Some(b"<<") {
        extract_dictionary(&page_object[resources_start..])?
    } else {
        let resources_ref = parse_indirect_ref(&page_object[resources_start..])?;
        extract_dictionary(find_object_by_ref(data, resources_ref)?)?
    };

    std::str::from_utf8(dict_bytes)
        .ok()
        .map(ToString::to_string)
}

fn extract_dictionary(data: &[u8]) -> Option<&[u8]> {
    let dict_start = skip_pdf_whitespace(data, 0);
    if data.get(dict_start..dict_start + 2) != Some(b"<<") {
        return None;
    }

    let mut depth = 0usize;
    let mut index = dict_start;
    while index + 1 < data.len() {
        match data.get(index..index + 2) {
            Some(b"<<") => {
                depth += 1;
                index += 2;
            }
            Some(b">>") => {
                depth = depth.checked_sub(1)?;
                index += 2;
                if depth == 0 {
                    return Some(&data[dict_start..index]);
                }
            }
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

pub fn parse_png_metadata(data: &[u8]) -> Option<ImageMetadata> {
    if !has_png_signature(data) || data.len() < 33 {
        return None;
    }

    let ihdr_length = read_u32_be(data, 8)?;
    if ihdr_length != 13 || &data[12..16] != b"IHDR" {
        return None;
    }

    let width = read_u32_be(data, 16)?;
    let height = read_u32_be(data, 20)?;
    let bit_depth = *data.get(24)?;
    let color_type = *data.get(25)?;
    let compression_method = *data.get(26)?;
    let filter_method = *data.get(27)?;
    let interlace_method = *data.get(28)?;
    if compression_method != 0 || filter_method != 0 || interlace_method != 0 {
        return None;
    }

    let color_space = match color_type {
        0 => ImageColorSpace::DeviceGray,
        2 => ImageColorSpace::DeviceRGB,
        _ => return None,
    };

    Some(ImageMetadata {
        width,
        height,
        color_space,
        bits_per_component: bit_depth,
    })
}

pub fn parse_jpeg_metadata(data: &[u8]) -> Option<ImageMetadata> {
    if !data.starts_with(&[0xFF, 0xD8]) {
        return None;
    }

    let mut index = 2usize;
    while index + 3 < data.len() {
        if data[index] != 0xFF {
            index += 1;
            continue;
        }

        while index < data.len() && data[index] == 0xFF {
            index += 1;
        }
        let marker = *data.get(index)?;
        index += 1;

        if marker == 0xD9 || marker == 0xDA {
            break;
        }

        let segment_length = read_u16_be(data, index)? as usize;
        if segment_length < 2 || index + segment_length > data.len() {
            return None;
        }

        if matches!(marker, 0xC0 | 0xC2) {
            let precision = *data.get(index + 2)?;
            let height = read_u16_be(data, index + 3)? as u32;
            let width = read_u16_be(data, index + 5)? as u32;
            let component_count = *data.get(index + 7)?;
            let color_space = match component_count {
                1 => ImageColorSpace::DeviceGray,
                3 => ImageColorSpace::DeviceRGB,
                _ => return None,
            };

            return Some(ImageMetadata {
                width,
                height,
                color_space,
                bits_per_component: precision,
            });
        }

        index += segment_length;
    }

    None
}

pub fn extract_png_image_data(data: &[u8]) -> Option<Vec<u8>> {
    parse_png_metadata(data)?;

    let mut index = 8usize;
    let mut image_data = Vec::new();

    while index + 12 <= data.len() {
        let chunk_length = read_u32_be(data, index)? as usize;
        let chunk_type = data.get(index + 4..index + 8)?;
        let chunk_data_start = index + 8;
        let chunk_data_end = chunk_data_start.checked_add(chunk_length)?;
        let crc_end = chunk_data_end.checked_add(4)?;
        if crc_end > data.len() {
            return None;
        }

        match chunk_type {
            b"IDAT" => image_data.extend_from_slice(&data[chunk_data_start..chunk_data_end]),
            b"IEND" => return (!image_data.is_empty()).then_some(image_data),
            _ => {}
        }

        index = crc_end;
    }

    (!image_data.is_empty()).then_some(image_data)
}

fn pixels_to_points(value: u32) -> DimensionValue {
    DimensionValue(i64::from(value) * SCALED_POINTS_PER_POINT)
}

fn pdf_points_to_dimension(value: f64) -> Option<DimensionValue> {
    value
        .is_finite()
        .then(|| DimensionValue((value * SCALED_POINTS_PER_POINT as f64).round() as i64))
}

fn normalize_graphics_scene(
    scene: GraphicsScene,
) -> Option<(GraphicsScene, DimensionValue, DimensionValue)> {
    let bounds = scene_bounds(&scene)?;
    let min_x = bounds.0;
    let min_y = bounds.1;
    let width = pdf_points_to_dimension(bounds.2 - min_x)?;
    let height = pdf_points_to_dimension(bounds.3 - min_y)?;

    let nodes = scene
        .nodes
        .into_iter()
        .map(|node| translate_graphic_node(node, -min_x, -min_y))
        .collect();
    Some((GraphicsScene { nodes }, width, height))
}

fn scene_bounds(scene: &GraphicsScene) -> Option<(f64, f64, f64, f64)> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut has_bounds = false;

    for node in &scene.nodes {
        let Some((node_min_x, node_min_y, node_max_x, node_max_y)) = graphic_node_bounds(node)
        else {
            continue;
        };
        min_x = min_x.min(node_min_x);
        min_y = min_y.min(node_min_y);
        max_x = max_x.max(node_max_x);
        max_y = max_y.max(node_max_y);
        has_bounds = true;
    }

    has_bounds.then_some((min_x, min_y, max_x, max_y))
}

fn graphic_node_bounds(node: &GraphicNode) -> Option<(f64, f64, f64, f64)> {
    match node {
        GraphicNode::External(_) | GraphicNode::Pdf(_) => None,
        GraphicNode::Group(group) => group_bounds(group),
        GraphicNode::Vector(primitive) => vector_bounds(primitive),
        GraphicNode::Text(text) => text_bounds(text),
    }
}

fn group_bounds(group: &GraphicGroup) -> Option<(f64, f64, f64, f64)> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut has_bounds = false;

    for child in &group.children {
        let Some((child_min_x, child_min_y, child_max_x, child_max_y)) = graphic_node_bounds(child)
        else {
            continue;
        };
        min_x = min_x.min(child_min_x);
        min_y = min_y.min(child_min_y);
        max_x = max_x.max(child_max_x);
        max_y = max_y.max(child_max_y);
        has_bounds = true;
    }

    has_bounds.then(|| apply_transform_to_bounds((min_x, min_y, max_x, max_y), group.transform))
}

fn vector_bounds(primitive: &VectorPrimitive) -> Option<(f64, f64, f64, f64)> {
    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;
    let mut has_points = false;

    for segment in &primitive.path {
        match segment {
            PathSegment::MoveTo(point) | PathSegment::LineTo(point) => {
                min_x = min_x.min(point.x);
                min_y = min_y.min(point.y);
                max_x = max_x.max(point.x);
                max_y = max_y.max(point.y);
                has_points = true;
            }
            PathSegment::CurveTo {
                control1,
                control2,
                end,
            } => {
                for point in [control1, control2, end] {
                    min_x = min_x.min(point.x);
                    min_y = min_y.min(point.y);
                    max_x = max_x.max(point.x);
                    max_y = max_y.max(point.y);
                    has_points = true;
                }
            }
            PathSegment::ClosePath => continue,
        }
    }

    has_points.then_some((min_x, min_y, max_x, max_y))
}

fn text_bounds(text: &GraphicText) -> Option<(f64, f64, f64, f64)> {
    if text.content.is_empty() {
        return Some((
            text.position.x,
            text.position.y,
            text.position.x,
            text.position.y,
        ));
    }

    let width = text.content.chars().count() as f64 * 6.0;
    Some((
        text.position.x,
        text.position.y,
        text.position.x + width,
        text.position.y + 12.0,
    ))
}

fn translate_graphic_node(node: GraphicNode, dx: f64, dy: f64) -> GraphicNode {
    match node {
        GraphicNode::External(graphic) => GraphicNode::External(graphic),
        GraphicNode::Pdf(graphic) => GraphicNode::Pdf(graphic),
        GraphicNode::Group(group) => GraphicNode::Group(GraphicGroup {
            children: group.children,
            default_stroke: group.default_stroke,
            default_fill: group.default_fill,
            default_line_width: group.default_line_width,
            clip_path: group.clip_path,
            transform: Transform2D {
                x_shift: group.transform.x_shift + dx,
                y_shift: group.transform.y_shift + dy,
                scale: group.transform.scale,
                rotate: group.transform.rotate,
            },
        }),
        GraphicNode::Vector(primitive) => GraphicNode::Vector(VectorPrimitive {
            path: primitive
                .path
                .into_iter()
                .map(|segment| translate_path_segment(segment, dx, dy))
                .collect(),
            stroke: primitive.stroke,
            fill: primitive.fill,
            line_width: primitive.line_width,
            arrows: primitive.arrows,
            dash_pattern: primitive.dash_pattern,
            line_cap: primitive.line_cap,
            line_join: primitive.line_join,
            opacity: primitive.opacity,
            fill_opacity: primitive.fill_opacity,
        }),
        GraphicNode::Text(text) => GraphicNode::Text(GraphicText {
            position: translate_point(text.position, dx, dy),
            content: text.content,
        }),
    }
}

fn translate_path_segment(segment: PathSegment, dx: f64, dy: f64) -> PathSegment {
    match segment {
        PathSegment::MoveTo(point) => PathSegment::MoveTo(translate_point(point, dx, dy)),
        PathSegment::LineTo(point) => PathSegment::LineTo(translate_point(point, dx, dy)),
        PathSegment::CurveTo {
            control1,
            control2,
            end,
        } => PathSegment::CurveTo {
            control1: translate_point(control1, dx, dy),
            control2: translate_point(control2, dx, dy),
            end: translate_point(end, dx, dy),
        },
        PathSegment::ClosePath => PathSegment::ClosePath,
    }
}

fn translate_point(point: Point, dx: f64, dy: f64) -> Point {
    Point {
        x: point.x + dx,
        y: point.y + dy,
    }
}

fn apply_transform_to_bounds(
    (min_x, min_y, max_x, max_y): (f64, f64, f64, f64),
    transform: Transform2D,
) -> (f64, f64, f64, f64) {
    let corners = [
        transform_point(Point { x: min_x, y: min_y }, transform),
        transform_point(Point { x: min_x, y: max_y }, transform),
        transform_point(Point { x: max_x, y: min_y }, transform),
        transform_point(Point { x: max_x, y: max_y }, transform),
    ];

    let mut transformed_min_x = f64::INFINITY;
    let mut transformed_min_y = f64::INFINITY;
    let mut transformed_max_x = f64::NEG_INFINITY;
    let mut transformed_max_y = f64::NEG_INFINITY;
    for point in corners {
        transformed_min_x = transformed_min_x.min(point.x);
        transformed_min_y = transformed_min_y.min(point.y);
        transformed_max_x = transformed_max_x.max(point.x);
        transformed_max_y = transformed_max_y.max(point.y);
    }

    (
        transformed_min_x,
        transformed_min_y,
        transformed_max_x,
        transformed_max_y,
    )
}

fn transform_point(point: Point, transform: Transform2D) -> Point {
    let radians = transform.rotate.to_radians();
    let cos_theta = radians.cos();
    let sin_theta = radians.sin();
    let scaled_x = point.x * transform.scale;
    let scaled_y = point.y * transform.scale;

    Point {
        x: (scaled_x * cos_theta) - (scaled_y * sin_theta) + transform.x_shift,
        y: (scaled_x * sin_theta) + (scaled_y * cos_theta) + transform.y_shift,
    }
}

fn graphic_layout(
    graphic: ResolvedGraphic,
) -> Option<(GraphicNode, DimensionValue, DimensionValue)> {
    match graphic {
        ResolvedGraphic::Raster(graphic) => {
            if graphic.metadata.width == 0 || graphic.metadata.height == 0 {
                return None;
            }
            Some((
                GraphicNode::External(graphic.clone()),
                pixels_to_points(graphic.metadata.width),
                pixels_to_points(graphic.metadata.height),
            ))
        }
        ResolvedGraphic::Pdf(graphic) => {
            let [llx, lly, urx, ury] = graphic.metadata.media_box;
            let natural_width = pdf_points_to_dimension(urx - llx)?;
            let natural_height = pdf_points_to_dimension(ury - lly)?;
            if natural_width.0 <= 0 || natural_height.0 <= 0 {
                return None;
            }
            Some((GraphicNode::Pdf(graphic), natural_width, natural_height))
        }
    }
}

fn scale_dimension(value: DimensionValue, factor: f64) -> DimensionValue {
    DimensionValue((value.0 as f64 * factor).round() as i64)
}

fn has_png_signature(data: &[u8]) -> bool {
    data.starts_with(&PNG_SIGNATURE)
}

fn read_u32_be(data: &[u8], offset: usize) -> Option<u32> {
    let bytes = data.get(offset..offset + 4)?;
    Some(u32::from_be_bytes(bytes.try_into().ok()?))
}

fn read_u16_be(data: &[u8], offset: usize) -> Option<u16> {
    let bytes = data.get(offset..offset + 2)?;
    Some(u16::from_be_bytes(bytes.try_into().ok()?))
}

pub use super::tikz::{parse_tikzpicture, TikzDiagnostic, TikzParseResult};

#[cfg(test)]
mod tests {
    use crate::assets::api::{AssetHandle, LogicalAssetId};
    use crate::kernel::api::StableId;

    use super::{
        compile_graphics_scene, compile_includegraphics, extract_png_image_data, is_pdf_signature,
        parse_image_metadata, parse_jpeg_metadata, parse_pdf_metadata, parse_png_metadata, Color,
        ExternalGraphic, GraphicAssetResolver, GraphicGroup, GraphicNode, GraphicText,
        GraphicsScene, ImageColorSpace, ImageMetadata, PathSegment, PdfGraphic, PdfGraphicMetadata,
        Point, ResolvedGraphic, Transform2D, VectorPrimitive,
    };
    use crate::kernel::api::DimensionValue;
    use crate::parser::api::IncludeGraphicsOptions;

    const PNG_1X1_RGB: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 2,
        0, 0, 0, 144, 119, 83, 222, 0, 0, 0, 12, 73, 68, 65, 84, 120, 156, 99, 248, 207, 192, 0, 0,
        3, 1, 1, 0, 201, 254, 146, 239, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    const JPEG_1X1_RGB_HEADER: &[u8] = &[
        255, 216, 255, 224, 0, 16, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 255, 192, 0, 17, 8, 0,
        1, 0, 1, 3, 1, 17, 0, 2, 17, 0, 3, 17, 0, 255, 217,
    ];
    const MINIMAL_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /ProcSet [/PDF] >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 18 >>\nstream\n0 0 m\n200 100 l\nS\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n";
    const CORRUPT_PDF: &[u8] =
        b"%PDF-1.4\n1 0 obj\n<< /Type /Page /MediaBox [0 0 200 100] /Contents 2 0 R >>\nendobj\n";
    const FILTERED_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /ProcSet [/PDF] >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 18 /Filter /FlateDecode >>\nstream\n0 0 m\n200 100 l\nS\nendstream\nendobj\n";

    struct StubGraphicResolver;

    impl GraphicAssetResolver for StubGraphicResolver {
        fn resolve(&self, path: &str) -> Option<ResolvedGraphic> {
            Some(ResolvedGraphic::Raster(ExternalGraphic {
                path: path.to_string(),
                asset_handle: AssetHandle {
                    id: LogicalAssetId(StableId(7)),
                },
                metadata: ImageMetadata {
                    width: 10,
                    height: 20,
                    color_space: ImageColorSpace::DeviceRGB,
                    bits_per_component: 8,
                },
            }))
        }
    }

    struct StubPdfGraphicResolver;

    impl GraphicAssetResolver for StubPdfGraphicResolver {
        fn resolve(&self, path: &str) -> Option<ResolvedGraphic> {
            Some(ResolvedGraphic::Pdf(PdfGraphic {
                path: path.to_string(),
                asset_handle: AssetHandle {
                    id: LogicalAssetId(StableId(8)),
                },
                metadata: PdfGraphicMetadata {
                    media_box: [0.0, 0.0, 200.0, 100.0],
                    page_data: b"0 0 m\n200 100 l\nS".to_vec(),
                    resources_dict: Some("<< /ProcSet [/PDF] >>".to_string()),
                },
            }))
        }
    }

    #[test]
    fn parses_png_metadata_from_ihdr() {
        assert_eq!(
            parse_png_metadata(PNG_1X1_RGB),
            Some(ImageMetadata {
                width: 1,
                height: 1,
                color_space: ImageColorSpace::DeviceRGB,
                bits_per_component: 8,
            })
        );
    }

    #[test]
    fn parses_jpeg_metadata_from_sof_segment() {
        assert_eq!(
            parse_jpeg_metadata(JPEG_1X1_RGB_HEADER),
            Some(ImageMetadata {
                width: 1,
                height: 1,
                color_space: ImageColorSpace::DeviceRGB,
                bits_per_component: 8,
            })
        );
    }

    #[test]
    fn parse_image_metadata_dispatches_on_signature() {
        assert_eq!(
            parse_image_metadata(PNG_1X1_RGB),
            parse_png_metadata(PNG_1X1_RGB)
        );
        assert_eq!(
            parse_image_metadata(JPEG_1X1_RGB_HEADER),
            parse_jpeg_metadata(JPEG_1X1_RGB_HEADER)
        );
    }

    #[test]
    fn detects_pdf_signature() {
        assert!(is_pdf_signature(MINIMAL_PDF));
        assert!(!is_pdf_signature(PNG_1X1_RGB));
        assert!(!is_pdf_signature(JPEG_1X1_RGB_HEADER));
    }

    #[test]
    fn parses_pdf_media_box_metadata() {
        assert_eq!(
            parse_pdf_metadata(MINIMAL_PDF),
            Some(PdfGraphicMetadata {
                media_box: [0.0, 0.0, 200.0, 100.0],
                page_data: b"0 0 m\n200 100 l\nS".to_vec(),
                resources_dict: Some("<< /ProcSet [/PDF] >>".to_string()),
            })
        );
    }

    #[test]
    fn parse_pdf_metadata_rejects_corrupt_input() {
        assert_eq!(parse_pdf_metadata(CORRUPT_PDF), None);
    }

    #[test]
    fn parse_pdf_metadata_rejects_filtered_streams() {
        assert_eq!(parse_pdf_metadata(FILTERED_PDF), None);
    }

    #[test]
    fn extracts_png_idat_payload() {
        let image_data = extract_png_image_data(PNG_1X1_RGB).expect("png idat");

        assert_eq!(
            image_data,
            vec![120, 156, 99, 248, 207, 192, 0, 0, 3, 1, 1, 0]
        );
    }

    #[test]
    fn compile_includegraphics_applies_width_and_scale() {
        let graphics_box = compile_includegraphics(
            "image.png",
            &IncludeGraphicsOptions {
                width: Some(DimensionValue(100 * 65_536)),
                height: None,
                scale: Some(2.0),
            },
            &StubGraphicResolver,
        )
        .expect("graphics box");

        assert_eq!(graphics_box.width, DimensionValue(200 * 65_536));
        assert_eq!(graphics_box.height, DimensionValue(400 * 65_536));
        assert!(graphics_box.scene.is_some());
    }

    #[test]
    fn compile_includegraphics_sizes_pdf_using_media_box() {
        let graphics_box = compile_includegraphics(
            "figure.pdf",
            &IncludeGraphicsOptions::default(),
            &StubPdfGraphicResolver,
        )
        .expect("graphics box");

        assert_eq!(graphics_box.width, DimensionValue(200 * 65_536));
        assert_eq!(graphics_box.height, DimensionValue(100 * 65_536));
        assert!(matches!(
            graphics_box
                .scene
                .as_ref()
                .and_then(|scene| scene.nodes.first()),
            Some(GraphicNode::Pdf(_))
        ));
    }

    #[test]
    fn compile_includegraphics_applies_options_to_pdf() {
        let graphics_box = compile_includegraphics(
            "figure.pdf",
            &IncludeGraphicsOptions {
                width: Some(DimensionValue(300 * 65_536)),
                height: None,
                scale: Some(0.5),
            },
            &StubPdfGraphicResolver,
        )
        .expect("graphics box");

        assert_eq!(graphics_box.width, DimensionValue(150 * 65_536));
        assert_eq!(graphics_box.height, DimensionValue(75 * 65_536));
    }

    #[test]
    fn compile_graphics_scene_normalizes_vector_and_text_bounds() {
        let graphics_box = compile_graphics_scene(GraphicsScene {
            nodes: vec![
                GraphicNode::Vector(VectorPrimitive {
                    path: vec![
                        PathSegment::MoveTo(Point { x: 10.0, y: 20.0 }),
                        PathSegment::LineTo(Point { x: 30.0, y: 20.0 }),
                    ],
                    stroke: Some(Color {
                        r: 0.0,
                        g: 0.0,
                        b: 0.0,
                    }),
                    fill: None,
                    line_width: 0.4,
                    ..Default::default()
                }),
                GraphicNode::Text(GraphicText {
                    position: Point { x: 12.0, y: 24.0 },
                    content: "Hi".to_string(),
                }),
            ],
        });

        assert_eq!(graphics_box.width, DimensionValue(20 * 65_536));
        assert_eq!(graphics_box.height, DimensionValue(16 * 65_536));
        assert_eq!(
            graphics_box.scene,
            Some(GraphicsScene {
                nodes: vec![
                    GraphicNode::Vector(VectorPrimitive {
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
                        ..Default::default()
                    }),
                    GraphicNode::Text(GraphicText {
                        position: Point { x: 2.0, y: 4.0 },
                        content: "Hi".to_string(),
                    }),
                ],
            })
        );
    }

    #[test]
    fn compile_graphics_scene_normalizes_group_bounds() {
        let graphics_box = compile_graphics_scene(GraphicsScene {
            nodes: vec![GraphicNode::Group(GraphicGroup {
                children: vec![GraphicNode::Vector(VectorPrimitive {
                    path: vec![
                        PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                        PathSegment::LineTo(Point { x: 20.0, y: 0.0 }),
                        PathSegment::LineTo(Point { x: 20.0, y: 10.0 }),
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
                default_stroke: None,
                default_fill: None,
                default_line_width: Some(0.4),
                clip_path: None,
                transform: Transform2D {
                    x_shift: 10.0,
                    y_shift: 20.0,
                    scale: 2.0,
                    rotate: 0.0,
                },
            })],
        });

        assert_eq!(graphics_box.width, DimensionValue(40 * 65_536));
        assert_eq!(graphics_box.height, DimensionValue(20 * 65_536));
        assert_eq!(
            graphics_box.scene,
            Some(GraphicsScene {
                nodes: vec![GraphicNode::Group(GraphicGroup {
                    children: vec![GraphicNode::Vector(VectorPrimitive {
                        path: vec![
                            PathSegment::MoveTo(Point { x: 0.0, y: 0.0 }),
                            PathSegment::LineTo(Point { x: 20.0, y: 0.0 }),
                            PathSegment::LineTo(Point { x: 20.0, y: 10.0 }),
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
                    default_stroke: None,
                    default_fill: None,
                    default_line_width: Some(0.4),
                    clip_path: None,
                    transform: Transform2D {
                        x_shift: 0.0,
                        y_shift: 0.0,
                        scale: 2.0,
                        rotate: 0.0,
                    },
                })],
            })
        );
    }
}
