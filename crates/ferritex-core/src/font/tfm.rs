use crate::kernel::api::DimensionValue;
use thiserror::Error;

const DIRECTORY_HALFWORDS: usize = 12;
const DIRECTORY_BYTES: usize = DIRECTORY_HALFWORDS * 2;
const DIRECTORY_WORDS: usize = 6;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TfmError {
    #[error("TFM data is truncated: expected {expected} bytes, got {actual}")]
    Truncated { expected: usize, actual: usize },
    #[error("invalid TFM table size: {description}")]
    InvalidTableSize { description: String },
    #[error("character code {code} is outside [{bc}, {ec}]")]
    CharCodeOutOfRange { code: u16, bc: u16, ec: u16 },
    #[error("invalid {table} index {index} for table length {len}")]
    InvalidIndex {
        table: &'static str,
        index: usize,
        len: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CharInfo {
    pub width_index: u8,
    pub height_index: u8,
    pub depth_index: u8,
    pub italic_index: u8,
    pub tag: u8,
    pub remainder: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TfmMetrics {
    pub design_size: DimensionValue,
    pub checksum: u32,
    pub bc: u16,
    pub ec: u16,
    pub char_infos: Vec<CharInfo>,
    pub widths: Vec<i32>,
    pub heights: Vec<i32>,
    pub depths: Vec<i32>,
    pub italic_corrections: Vec<i32>,
    // TODO: Parse lig_kern table.
    // TODO: Parse kern table.
    // TODO: Parse extensible recipes.
    // TODO: Parse font parameters.
    design_size_fixword: i32,
}

impl TfmMetrics {
    pub fn parse(data: &[u8]) -> Result<TfmMetrics, TfmError> {
        if data.len() < DIRECTORY_BYTES {
            return Err(TfmError::Truncated {
                expected: DIRECTORY_BYTES,
                actual: data.len(),
            });
        }

        let lf = usize::from(read_u16(data, 0)?);
        let lh = usize::from(read_u16(data, 2)?);
        let bc = read_u16(data, 4)?;
        let ec = read_u16(data, 6)?;
        let nw = usize::from(read_u16(data, 8)?);
        let nh = usize::from(read_u16(data, 10)?);
        let nd = usize::from(read_u16(data, 12)?);
        let ni = usize::from(read_u16(data, 14)?);
        let nl = usize::from(read_u16(data, 16)?);
        let nk = usize::from(read_u16(data, 18)?);
        let ne = usize::from(read_u16(data, 20)?);
        let np = usize::from(read_u16(data, 22)?);

        if lh < 2 {
            return Err(TfmError::InvalidTableSize {
                description: format!("header length must be at least 2 words, got {lh}"),
            });
        }

        let char_count = usize::from(
            ec.checked_sub(bc)
                .and_then(|span| span.checked_add(1))
                .ok_or_else(|| TfmError::InvalidTableSize {
                    description: format!("invalid character range: bc={bc}, ec={ec}"),
                })?,
        );

        let expected_lf = DIRECTORY_WORDS + lh + char_count + nw + nh + nd + ni + nl + nk + ne + np;
        if lf != expected_lf {
            return Err(TfmError::InvalidTableSize {
                description: format!(
                    "directory word count mismatch: lf={lf}, expected {expected_lf}"
                ),
            });
        }

        let expected_bytes = lf * 4;
        if data.len() < expected_bytes {
            return Err(TfmError::Truncated {
                expected: expected_bytes,
                actual: data.len(),
            });
        }
        if data.len() != expected_bytes {
            return Err(TfmError::InvalidTableSize {
                description: format!(
                    "file length mismatch: expected {expected_bytes} bytes, got {}",
                    data.len()
                ),
            });
        }

        let header_offset = DIRECTORY_WORDS * 4;
        let checksum = read_u32(data, header_offset)?;
        let design_size_fixword = read_i32(data, header_offset + 4)?;
        let design_size = DimensionValue((design_size_fixword as i64) >> 4);

        let char_info_offset = header_offset + lh * 4;
        let width_offset = char_info_offset + char_count * 4;
        let height_offset = width_offset + nw * 4;
        let depth_offset = height_offset + nh * 4;
        let italic_offset = depth_offset + nd * 4;

        let char_infos = read_char_infos(data, char_info_offset, char_count)?;
        let widths = read_fix_words(data, width_offset, nw)?;
        let heights = read_fix_words(data, height_offset, nh)?;
        let depths = read_fix_words(data, depth_offset, nd)?;
        let italic_corrections = read_fix_words(data, italic_offset, ni)?;

        Ok(TfmMetrics {
            design_size,
            checksum,
            bc,
            ec,
            char_infos,
            widths,
            heights,
            depths,
            italic_corrections,
            design_size_fixword,
        })
    }

    pub fn width(&self, char_code: u16) -> Result<DimensionValue, TfmError> {
        let char_info = self.char_info(char_code)?;
        if char_info.width_index == 0 {
            return Ok(DimensionValue::zero());
        }
        self.metric_from_table("widths", usize::from(char_info.width_index), &self.widths)
    }

    pub fn height(&self, char_code: u16) -> Result<DimensionValue, TfmError> {
        let char_info = self.char_info(char_code)?;
        self.metric_from_table(
            "heights",
            usize::from(char_info.height_index),
            &self.heights,
        )
    }

    pub fn depth(&self, char_code: u16) -> Result<DimensionValue, TfmError> {
        let char_info = self.char_info(char_code)?;
        self.metric_from_table("depths", usize::from(char_info.depth_index), &self.depths)
    }

    pub fn italic_correction(&self, char_code: u16) -> Result<DimensionValue, TfmError> {
        let char_info = self.char_info(char_code)?;
        self.metric_from_table(
            "italic_corrections",
            usize::from(char_info.italic_index),
            &self.italic_corrections,
        )
    }

    fn char_info(&self, char_code: u16) -> Result<&CharInfo, TfmError> {
        if char_code < self.bc || char_code > self.ec {
            return Err(TfmError::CharCodeOutOfRange {
                code: char_code,
                bc: self.bc,
                ec: self.ec,
            });
        }

        Ok(&self.char_infos[usize::from(char_code - self.bc)])
    }

    fn metric_from_table(
        &self,
        table: &'static str,
        index: usize,
        values: &[i32],
    ) -> Result<DimensionValue, TfmError> {
        let metric_fixword = values.get(index).ok_or(TfmError::InvalidIndex {
            table,
            index,
            len: values.len(),
        })?;
        Ok(self.fix_word_to_dimension(*metric_fixword))
    }

    fn fix_word_to_dimension(&self, metric_fixword: i32) -> DimensionValue {
        DimensionValue((self.design_size_fixword as i64 * metric_fixword as i64) >> 24)
    }
}

fn read_char_infos(data: &[u8], offset: usize, count: usize) -> Result<Vec<CharInfo>, TfmError> {
    let bytes = read_slice(data, offset, count * 4)?;
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| CharInfo {
            width_index: chunk[0],
            height_index: chunk[1] >> 4,
            depth_index: chunk[1] & 0x0f,
            italic_index: chunk[2] >> 2,
            tag: chunk[2] & 0x03,
            remainder: chunk[3],
        })
        .collect())
}

fn read_fix_words(data: &[u8], offset: usize, count: usize) -> Result<Vec<i32>, TfmError> {
    let bytes = read_slice(data, offset, count * 4)?;
    Ok(bytes
        .chunks_exact(4)
        .map(|chunk| i32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, TfmError> {
    let bytes = read_slice(data, offset, 2)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, TfmError> {
    let bytes = read_slice(data, offset, 4)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_i32(data: &[u8], offset: usize) -> Result<i32, TfmError> {
    let bytes = read_slice(data, offset, 4)?;
    Ok(i32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_slice(data: &[u8], offset: usize, len: usize) -> Result<&[u8], TfmError> {
    let end = offset.saturating_add(len);
    data.get(offset..end).ok_or(TfmError::Truncated {
        expected: end,
        actual: data.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::{CharInfo, TfmError, TfmMetrics};
    use crate::kernel::api::DimensionValue;

    const TEN_PT_FIXWORD: i32 = 10_485_760;

    #[test]
    fn parses_metrics_and_reads_dimensions_for_two_characters() {
        let data = build_tfm(TestTfm {
            lf_override: None,
            bc: 65,
            ec: 66,
            checksum: 0x1234_5678,
            design_size_fixword: TEN_PT_FIXWORD,
            char_infos: &[
                CharInfo {
                    width_index: 1,
                    height_index: 1,
                    depth_index: 1,
                    italic_index: 1,
                    tag: 0,
                    remainder: 0,
                },
                CharInfo {
                    width_index: 2,
                    height_index: 0,
                    depth_index: 0,
                    italic_index: 0,
                    tag: 0,
                    remainder: 0,
                },
            ],
            widths: &[0, 349_525, 524_288],
            heights: &[0, 104_858],
            depths: &[0, 52_429],
            italics: &[0, 131_072],
        });

        let metrics = TfmMetrics::parse(&data).expect("parse TFM");

        assert_eq!(metrics.design_size, DimensionValue(655_360));
        assert_eq!(metrics.checksum, 0x1234_5678);
        assert_eq!(metrics.width(65), Ok(DimensionValue(218_453)));
        assert_eq!(metrics.width(66), Ok(DimensionValue(327_680)));
        assert_eq!(metrics.height(65), Ok(DimensionValue(65_536)));
        assert_eq!(metrics.depth(65), Ok(DimensionValue(32_768)));
        assert_eq!(metrics.italic_correction(65), Ok(DimensionValue(81_920)));
    }

    #[test]
    fn returns_truncated_for_short_directory() {
        let error = TfmMetrics::parse(&[0; 23]).expect_err("short directory should fail");

        assert_eq!(
            error,
            TfmError::Truncated {
                expected: 24,
                actual: 23,
            }
        );
    }

    #[test]
    fn returns_invalid_table_size_for_mismatched_lf() {
        let data = build_tfm(TestTfm {
            lf_override: Some(16),
            bc: 65,
            ec: 66,
            checksum: 0,
            design_size_fixword: TEN_PT_FIXWORD,
            char_infos: &[
                CharInfo {
                    width_index: 1,
                    height_index: 0,
                    depth_index: 0,
                    italic_index: 0,
                    tag: 0,
                    remainder: 0,
                },
                CharInfo {
                    width_index: 1,
                    height_index: 0,
                    depth_index: 0,
                    italic_index: 0,
                    tag: 0,
                    remainder: 0,
                },
            ],
            widths: &[0, 32],
            heights: &[0],
            depths: &[0],
            italics: &[0],
        });

        let error = TfmMetrics::parse(&data).expect_err("lf mismatch should fail");

        assert!(matches!(error, TfmError::InvalidTableSize { .. }));
    }

    #[test]
    fn returns_char_code_out_of_range_for_missing_character() {
        let data = build_tfm(TestTfm {
            lf_override: None,
            bc: 65,
            ec: 66,
            checksum: 0,
            design_size_fixword: TEN_PT_FIXWORD,
            char_infos: &[
                CharInfo {
                    width_index: 1,
                    height_index: 0,
                    depth_index: 0,
                    italic_index: 0,
                    tag: 0,
                    remainder: 0,
                },
                CharInfo {
                    width_index: 1,
                    height_index: 0,
                    depth_index: 0,
                    italic_index: 0,
                    tag: 0,
                    remainder: 0,
                },
            ],
            widths: &[0, 32],
            heights: &[0],
            depths: &[0],
            italics: &[0],
        });
        let metrics = TfmMetrics::parse(&data).expect("parse TFM");

        let error = metrics.width(64).expect_err("out of range should fail");

        assert_eq!(
            error,
            TfmError::CharCodeOutOfRange {
                code: 64,
                bc: 65,
                ec: 66,
            }
        );
    }

    #[test]
    fn returns_zero_width_when_width_index_is_zero() {
        let data = build_tfm(TestTfm {
            lf_override: None,
            bc: 65,
            ec: 65,
            checksum: 0,
            design_size_fixword: TEN_PT_FIXWORD,
            char_infos: &[CharInfo {
                width_index: 0,
                height_index: 0,
                depth_index: 0,
                italic_index: 0,
                tag: 0,
                remainder: 0,
            }],
            widths: &[123],
            heights: &[0],
            depths: &[0],
            italics: &[0],
        });
        let metrics = TfmMetrics::parse(&data).expect("parse TFM");

        assert_eq!(metrics.width(65), Ok(DimensionValue::zero()));
    }

    struct TestTfm<'a> {
        lf_override: Option<u16>,
        bc: u16,
        ec: u16,
        checksum: u32,
        design_size_fixword: i32,
        char_infos: &'a [CharInfo],
        widths: &'a [i32],
        heights: &'a [i32],
        depths: &'a [i32],
        italics: &'a [i32],
    }

    fn build_tfm(config: TestTfm<'_>) -> Vec<u8> {
        let char_count = usize::from(config.ec - config.bc + 1);
        assert_eq!(config.char_infos.len(), char_count);

        let lh = 2u16;
        let nw = u16::try_from(config.widths.len()).expect("widths length");
        let nh = u16::try_from(config.heights.len()).expect("heights length");
        let nd = u16::try_from(config.depths.len()).expect("depths length");
        let ni = u16::try_from(config.italics.len()).expect("italics length");
        let lf = config
            .lf_override
            .unwrap_or(6 + lh + u16::try_from(char_count).expect("char count") + nw + nh + nd + ni);

        let mut data = Vec::with_capacity(usize::from(lf) * 4);
        for value in [lf, lh, config.bc, config.ec, nw, nh, nd, ni, 0, 0, 0, 0] {
            data.extend_from_slice(&value.to_be_bytes());
        }
        data.extend_from_slice(&config.checksum.to_be_bytes());
        data.extend_from_slice(&config.design_size_fixword.to_be_bytes());
        for char_info in config.char_infos {
            data.push(char_info.width_index);
            data.push((char_info.height_index << 4) | (char_info.depth_index & 0x0f));
            data.push((char_info.italic_index << 2) | (char_info.tag & 0x03));
            data.push(char_info.remainder);
        }
        for table in [config.widths, config.heights, config.depths, config.italics] {
            for value in table {
                data.extend_from_slice(&value.to_be_bytes());
            }
        }

        data
    }
}
