use crate::assets::api::AssetHandle;
use crate::kernel::api::DimensionValue;
use crate::parser::api::IncludeGraphicsOptions;

const SCALED_POINTS_PER_POINT: i64 = 65_536;
const PNG_SIGNATURE: [u8; 8] = [137, 80, 78, 71, 13, 10, 26, 10];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageColorSpace {
    DeviceRGB,
    DeviceGray,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageMetadata {
    pub width: u32,
    pub height: u32,
    pub color_space: ImageColorSpace,
    pub bits_per_component: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalGraphic {
    pub path: String,
    pub asset_handle: AssetHandle,
    pub metadata: ImageMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GraphicNode {
    External(ExternalGraphic),
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct GraphicsScene {
    pub nodes: Vec<GraphicNode>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphicsBox {
    pub width: DimensionValue,
    pub height: DimensionValue,
    pub scene: Option<GraphicsScene>,
}

pub trait GraphicAssetResolver {
    fn resolve(&self, path: &str) -> Option<ExternalGraphic>;
}

pub fn compile_includegraphics(
    path: &str,
    options: &IncludeGraphicsOptions,
    resolver: &dyn GraphicAssetResolver,
) -> Option<GraphicsBox> {
    let graphic = resolver.resolve(path)?;
    if graphic.metadata.width == 0 || graphic.metadata.height == 0 {
        return None;
    }

    let natural_width = pixels_to_points(graphic.metadata.width);
    let natural_height = pixels_to_points(graphic.metadata.height);
    let aspect_ratio = graphic.metadata.height as f64 / graphic.metadata.width as f64;

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
        scene: Some(GraphicsScene {
            nodes: vec![GraphicNode::External(graphic)],
        }),
    })
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

#[cfg(test)]
mod tests {
    use crate::assets::api::{AssetHandle, LogicalAssetId};
    use crate::kernel::api::StableId;

    use super::{
        compile_includegraphics, extract_png_image_data, parse_image_metadata, parse_jpeg_metadata,
        parse_png_metadata, ExternalGraphic, GraphicAssetResolver, ImageColorSpace, ImageMetadata,
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

    struct StubGraphicResolver;

    impl GraphicAssetResolver for StubGraphicResolver {
        fn resolve(&self, path: &str) -> Option<ExternalGraphic> {
            Some(ExternalGraphic {
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
            })
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
}
