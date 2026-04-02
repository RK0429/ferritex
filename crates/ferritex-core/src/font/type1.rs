use thiserror::Error;

const PFB_MAGIC: u8 = 0x80;
const PFB_ASCII_SEGMENT: u8 = 0x01;
const PFB_BINARY_SEGMENT: u8 = 0x02;
const PFB_EOF_SEGMENT: u8 = 0x03;
const FALLBACK_FONT_NAME: &str = "UnknownType1Font";

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Type1Error {
    #[error("Type1 PFB data is truncated")]
    Truncated,
    #[error("invalid Type1 PFB magic byte {found:#04x} at offset {offset}")]
    InvalidMagic { offset: usize, found: u8 },
    #[error("invalid Type1 PFB segment type {segment_type:#04x} at offset {offset}")]
    InvalidSegmentType { offset: usize, segment_type: u8 },
    #[error("missing required Type1 PFB segments: ascii={ascii_present}, binary={binary_present}")]
    MissingSegments {
        ascii_present: bool,
        binary_present: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Type1Font {
    pub font_name: String,
    pub header_segment: Vec<u8>,
    pub binary_segment: Vec<u8>,
    pub trailer_segment: Vec<u8>,
}

impl Type1Font {
    pub const FALLBACK_FONT_NAME: &'static str = FALLBACK_FONT_NAME;

    pub fn parse(data: &[u8]) -> Result<Self, Type1Error> {
        let mut offset = 0usize;
        let mut header_segment = Vec::new();
        let mut binary_segment = Vec::new();
        let mut trailer_segment = Vec::new();
        let mut saw_eof = false;
        let mut saw_binary_segment = false;

        while offset < data.len() {
            let segment_offset = offset;
            let magic = *data.get(offset).ok_or(Type1Error::Truncated)?;
            if magic != PFB_MAGIC {
                return Err(Type1Error::InvalidMagic {
                    offset,
                    found: magic,
                });
            }

            let segment_type = *data.get(offset + 1).ok_or(Type1Error::Truncated)?;
            if segment_type == PFB_EOF_SEGMENT {
                saw_eof = true;
                break;
            }

            let length = u32::from_le_bytes(
                data.get(offset + 2..offset + 6)
                    .ok_or(Type1Error::Truncated)?
                    .try_into()
                    .expect("slice length verified"),
            ) as usize;
            offset += 6;

            match segment_type {
                PFB_ASCII_SEGMENT => {
                    let segment = data
                        .get(offset..offset + length)
                        .ok_or(Type1Error::Truncated)?;
                    if saw_binary_segment {
                        trailer_segment.extend_from_slice(segment);
                    } else {
                        header_segment.extend_from_slice(segment);
                    }
                    offset += length;
                }
                PFB_BINARY_SEGMENT => {
                    let segment = data
                        .get(offset..offset + length)
                        .ok_or(Type1Error::Truncated)?;
                    binary_segment.extend_from_slice(segment);
                    saw_binary_segment = true;
                    offset += length;
                }
                _ => {
                    return Err(Type1Error::InvalidSegmentType {
                        offset: segment_offset,
                        segment_type,
                    });
                }
            }
        }

        if !saw_eof {
            return Err(Type1Error::Truncated);
        }

        if header_segment.is_empty() || binary_segment.is_empty() {
            return Err(Type1Error::MissingSegments {
                ascii_present: !header_segment.is_empty(),
                binary_present: !binary_segment.is_empty(),
            });
        }

        Ok(Self {
            font_name: extract_font_name(&header_segment)
                .unwrap_or_else(|| Self::FALLBACK_FONT_NAME.to_string()),
            header_segment,
            binary_segment,
            trailer_segment,
        })
    }
}

fn extract_font_name(ascii_segment: &[u8]) -> Option<String> {
    let marker = b"/FontName";
    let mut offset = 0usize;

    while let Some(relative_index) = ascii_segment[offset..]
        .windows(marker.len())
        .position(|window| window == marker)
    {
        let mut cursor = offset + relative_index + marker.len();
        while matches!(ascii_segment.get(cursor), Some(byte) if byte.is_ascii_whitespace()) {
            cursor += 1;
        }
        if ascii_segment.get(cursor) != Some(&b'/') {
            offset = cursor;
            continue;
        }
        cursor += 1;

        let start = cursor;
        while let Some(byte) = ascii_segment.get(cursor) {
            if byte.is_ascii_whitespace() || is_postscript_name_delimiter(*byte) {
                break;
            }
            cursor += 1;
        }

        if cursor > start {
            return Some(String::from_utf8_lossy(&ascii_segment[start..cursor]).into_owned());
        }
        offset = cursor;
    }

    None
}

fn is_postscript_name_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{Type1Error, Type1Font};

    fn pfb_segment(segment_type: u8, payload: &[u8]) -> Vec<u8> {
        let mut segment = vec![0x80, segment_type];
        segment.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        segment.extend_from_slice(payload);
        segment
    }

    fn build_pfb(header: &[u8], binary: &[u8], trailer: &[u8]) -> Vec<u8> {
        let mut pfb = pfb_segment(0x01, header);
        pfb.extend_from_slice(&pfb_segment(0x02, binary));
        if !trailer.is_empty() {
            pfb.extend_from_slice(&pfb_segment(0x01, trailer));
        }
        pfb.extend_from_slice(&[0x80, 0x03]);
        pfb
    }

    #[test]
    fn parses_valid_pfb_with_ascii_binary_and_eof_segments() {
        let header = b"%!FontType1-1.0: CMR10 003.002\n/FontName /CMR10 def\n";
        let binary = b"\x01\x02\x03\x04";
        let font = Type1Font::parse(&build_pfb(header, binary, b"")).expect("parse pfb");

        assert_eq!(font.font_name, "CMR10");
        assert_eq!(font.header_segment, header);
        assert_eq!(font.binary_segment, binary);
        assert!(font.trailer_segment.is_empty());
    }

    #[test]
    fn separates_three_segment_pfb_into_header_binary_and_trailer() {
        let header = b"%!FontType1-1.0: CMR10 003.002\n/FontName /CMR10 def\n";
        let binary = b"\x01\x02\x03\x04";
        let trailer = b"0000000000\ncleartomark\n";
        let font = Type1Font::parse(&build_pfb(header, binary, trailer)).expect("parse pfb");

        assert_eq!(font.header_segment, header);
        assert_eq!(font.binary_segment, binary);
        assert_eq!(font.trailer_segment, trailer);
    }

    #[test]
    fn returns_truncated_error_for_short_segment_payload() {
        let ascii = b"/FontName /CMR10 def\n";
        let mut pfb = vec![0x80, 0x01];
        pfb.extend_from_slice(&(ascii.len() as u32 + 1).to_le_bytes());
        pfb.extend_from_slice(ascii);

        let error = Type1Font::parse(&pfb).expect_err("truncated pfb should fail");

        assert_eq!(error, Type1Error::Truncated);
    }

    #[test]
    fn returns_invalid_magic_error_for_bad_header() {
        let error =
            Type1Font::parse(&[0x81, 0x01, 0, 0, 0, 0]).expect_err("invalid magic should fail");

        assert_eq!(
            error,
            Type1Error::InvalidMagic {
                offset: 0,
                found: 0x81,
            }
        );
    }

    #[test]
    fn returns_missing_segments_error_when_binary_segment_is_absent() {
        let ascii = b"/FontName /CMR10 def\n";
        let mut pfb = pfb_segment(0x01, ascii);
        pfb.extend_from_slice(&[0x80, 0x03]);

        let error = Type1Font::parse(&pfb).expect_err("missing binary segment should fail");

        assert_eq!(
            error,
            Type1Error::MissingSegments {
                ascii_present: true,
                binary_present: false,
            }
        );
    }

    #[test]
    fn falls_back_to_default_font_name_when_header_omits_fontname() {
        let header = b"%!FontType1-1.0: Unknown 001.000\n";
        let binary = b"\x0a\x0b";
        let font = Type1Font::parse(&build_pfb(header, binary, b"")).expect("parse pfb");

        assert_eq!(font.font_name, Type1Font::FALLBACK_FONT_NAME);
    }

    #[test]
    fn parses_real_cmr10_pfb_with_expected_segment_lengths() {
        let path = Path::new(
            "/usr/local/texlive/2025/texmf-dist/fonts/type1/public/amsfonts/cm/cmr10.pfb",
        );
        if !path.is_file() {
            return;
        }

        let font = Type1Font::parse(&fs::read(path).expect("read cmr10.pfb")).expect("parse pfb");

        assert_eq!(font.font_name, "CMR10");
        assert_eq!(font.header_segment.len(), 4_287);
        assert_eq!(font.binary_segment.len(), 30_900);
        assert_eq!(font.trailer_segment.len(), 545);
        assert!(font
            .trailer_segment
            .windows(b"cleartomark".len())
            .any(|window| window == b"cleartomark"));
    }
}
