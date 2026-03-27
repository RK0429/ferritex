use std::collections::BTreeSet;

use thiserror::Error;

const SFNT_HEADER_LEN: usize = 12;
const TABLE_RECORD_LEN: usize = 16;
const TRUETYPE_MAGIC: u32 = 0x0001_0000;
const OPENTYPE_CFF_MAGIC: u32 = 0x4f54_544f;
const HEAD_MAGIC: u32 = 0x5f0f_3cf5;
const HEAD_TABLE_LEN: usize = 54;
const HHEA_TABLE_LEN: usize = 36;
const MAXP_TABLE_MIN_LEN: usize = 6;
const CMAP_HEADER_LEN: usize = 4;
const CMAP_ENCODING_RECORD_LEN: usize = 8;
const CMAP_FORMAT_4_HEADER_LEN: usize = 14;
const NAME_HEADER_LEN: usize = 6;
const NAME_RECORD_LEN: usize = 12;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum OpenTypeError {
    #[error("OpenType data is truncated")]
    Truncated,
    #[error("invalid OpenType sfVersion magic")]
    InvalidMagic,
    #[error("required table not found: {tag}")]
    TableNotFound { tag: String },
    #[error("invalid {table} table data: {description}")]
    InvalidTableData {
        table: &'static str,
        description: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HeadTable {
    pub units_per_em: u16,
    pub index_to_loc_format: i16,
    pub flags: u16,
    pub x_min: i16,
    pub y_min: i16,
    pub x_max: i16,
    pub y_max: i16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HheaTable {
    pub ascender: i16,
    pub descender: i16,
    pub line_gap: i16,
    pub number_of_h_metrics: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MaxpTable {
    num_glyphs: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HorizontalMetric {
    advance_width: u16,
    lsb: i16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HmtxTable {
    h_metrics: Vec<HorizontalMetric>,
    left_side_bearings: Vec<i16>,
}

impl HmtxTable {
    pub fn advance_width(&self, glyph_id: u16) -> Option<u16> {
        let glyph_index = usize::from(glyph_id);
        if glyph_index < self.h_metrics.len() {
            return Some(self.h_metrics[glyph_index].advance_width);
        }
        if glyph_index < self.total_glyphs() {
            return self.h_metrics.last().map(|metric| metric.advance_width);
        }
        None
    }

    pub fn lsb(&self, glyph_id: u16) -> Option<i16> {
        let glyph_index = usize::from(glyph_id);
        if glyph_index < self.h_metrics.len() {
            return Some(self.h_metrics[glyph_index].lsb);
        }
        let extra_index = glyph_index.checked_sub(self.h_metrics.len())?;
        self.left_side_bearings.get(extra_index).copied()
    }

    fn total_glyphs(&self) -> usize {
        self.h_metrics.len() + self.left_side_bearings.len()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CmapFormat4Segment {
    start_code: u16,
    end_code: u16,
    id_delta: i16,
    id_range_offset: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CmapTable {
    segments: Vec<CmapFormat4Segment>,
    glyph_id_array: Vec<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct GlyphTable {
    loca_record: TableRecord,
    glyf_record: TableRecord,
    loca_offsets: Vec<u32>,
}

impl CmapTable {
    fn lookup(&self, codepoint: u32) -> Option<u16> {
        let code = u16::try_from(codepoint).ok()?;
        for (index, segment) in self.segments.iter().enumerate() {
            if code < segment.start_code || code > segment.end_code {
                continue;
            }

            if segment.id_range_offset == 0 {
                let glyph_id = code.wrapping_add(segment.id_delta as u16);
                return (glyph_id != 0).then_some(glyph_id);
            }

            let base_index = index + usize::from(segment.id_range_offset / 2);
            let glyph_array_index = base_index
                .checked_add(usize::from(code - segment.start_code))?
                .checked_sub(self.segments.len())?;
            let glyph_id = *self.glyph_id_array.get(glyph_array_index)?;
            if glyph_id == 0 {
                return None;
            }
            let adjusted = glyph_id.wrapping_add(segment.id_delta as u16);
            return (adjusted != 0).then_some(adjusted);
        }
        None
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenTypeFont {
    sf_version: u32,
    raw_data: Vec<u8>,
    table_directory: TableDirectory,
    head: HeadTable,
    hhea: HheaTable,
    maxp: MaxpTable,
    hmtx: HmtxTable,
    cmap: CmapTable,
    glyph_table: Option<GlyphTable>,
}

impl OpenTypeFont {
    pub fn parse(data: &[u8]) -> Result<Self, OpenTypeError> {
        let table_directory = parse_table_directory(data)?;
        let sf_version = read_u32(data, 0)?;
        let head = parse_head(table_directory.table_data(data, "head")?)?;
        let hhea = parse_hhea(table_directory.table_data(data, "hhea")?)?;
        let maxp = parse_maxp(table_directory.table_data(data, "maxp")?)?;
        let hmtx = parse_hmtx(
            table_directory.table_data(data, "hmtx")?,
            hhea.number_of_h_metrics,
            maxp.num_glyphs,
        )?;
        let cmap = parse_cmap(table_directory.table_data(data, "cmap")?)?;
        let glyph_table = parse_glyph_table(
            &table_directory,
            data,
            maxp.num_glyphs,
            head.index_to_loc_format,
        )?;

        Ok(Self {
            sf_version,
            raw_data: data.to_vec(),
            table_directory,
            head,
            hhea,
            maxp,
            hmtx,
            cmap,
            glyph_table,
        })
    }

    pub fn glyph_id(&self, codepoint: u32) -> Option<u16> {
        self.cmap.lookup(codepoint)
    }

    pub fn advance_width(&self, glyph_id: u16) -> Option<u16> {
        self.hmtx.advance_width(glyph_id)
    }

    pub fn units_per_em(&self) -> u16 {
        self.head.units_per_em
    }

    pub fn raw_data(&self) -> &[u8] {
        &self.raw_data
    }

    pub fn ascender(&self) -> i16 {
        self.hhea.ascender
    }

    pub fn descender(&self) -> i16 {
        self.hhea.descender
    }

    pub fn line_gap(&self) -> i16 {
        self.hhea.line_gap
    }

    pub fn bounding_box(&self) -> [i16; 4] {
        [
            self.head.x_min,
            self.head.y_min,
            self.head.x_max,
            self.head.y_max,
        ]
    }

    pub fn subset(&self, used_glyph_ids: &BTreeSet<u16>) -> Vec<u8> {
        self.try_subset(used_glyph_ids)
            .unwrap_or_else(|_| self.raw_data.clone())
    }

    fn try_subset(&self, used_glyph_ids: &BTreeSet<u16>) -> Result<Vec<u8>, OpenTypeError> {
        let glyph_table = match &self.glyph_table {
            Some(glyph_table) => glyph_table,
            None => return Ok(self.raw_data.clone()),
        };

        let glyf_data = self
            .table_directory
            .table_data(&self.raw_data, "glyf")
            .expect("glyf table must exist when glyph data is parsed");
        let (subset_glyf, subset_loca_offsets) =
            build_subset_glyf(glyf_data, &glyph_table.loca_offsets, used_glyph_ids)?;
        let subset_loca = build_loca_table(&subset_loca_offsets, self.head.index_to_loc_format)?;

        let tables = self
            .table_directory
            .tables
            .iter()
            .map(|record| {
                let data = if record.tag == *b"glyf" {
                    subset_glyf.clone()
                } else if record.tag == *b"loca" {
                    subset_loca.clone()
                } else {
                    self.table_directory
                        .table_data(
                            &self.raw_data,
                            std::str::from_utf8(&record.tag).expect("table tag must be ASCII"),
                        )
                        .expect("existing table must be readable")
                        .to_vec()
                };
                (record.tag, data)
            })
            .collect::<Vec<_>>();

        Ok(build_sfnt(self.sf_version, &tables))
    }
}

pub(crate) fn extract_font_names(data: &[u8]) -> Result<Vec<String>, OpenTypeError> {
    let table_directory = parse_table_directory(data)?;
    let Some(name_record) = table_directory.maybe_table_record("name") else {
        return Ok(Vec::new());
    };
    let table = read_slice(data, name_record.offset, name_record.length)?;

    parse_name_table(table)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TableRecord {
    tag: [u8; 4],
    offset: usize,
    length: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TableDirectory {
    tables: Vec<TableRecord>,
}

impl TableDirectory {
    fn table_data<'a>(&self, data: &'a [u8], tag: &str) -> Result<&'a [u8], OpenTypeError> {
        let table = self.table_record(tag)?;
        read_slice(data, table.offset, table.length)
    }

    fn table_record(&self, tag: &str) -> Result<&TableRecord, OpenTypeError> {
        let tag_bytes = tag_to_bytes(tag);
        self.tables
            .iter()
            .find(|record| record.tag == tag_bytes)
            .ok_or_else(|| OpenTypeError::TableNotFound {
                tag: tag.to_string(),
            })
    }

    fn maybe_table_record(&self, tag: &str) -> Option<&TableRecord> {
        let tag_bytes = tag_to_bytes(tag);
        self.tables.iter().find(|record| record.tag == tag_bytes)
    }
}

fn parse_table_directory(data: &[u8]) -> Result<TableDirectory, OpenTypeError> {
    if data.len() < SFNT_HEADER_LEN {
        return Err(OpenTypeError::Truncated);
    }

    let sf_version = read_u32(data, 0)?;
    if sf_version != TRUETYPE_MAGIC && sf_version != OPENTYPE_CFF_MAGIC {
        return Err(OpenTypeError::InvalidMagic);
    }

    let num_tables = usize::from(read_u16(data, 4)?);
    let records = read_slice(
        data,
        SFNT_HEADER_LEN,
        num_tables
            .checked_mul(TABLE_RECORD_LEN)
            .ok_or(OpenTypeError::Truncated)?,
    )?;

    let tables = records
        .chunks_exact(TABLE_RECORD_LEN)
        .map(|chunk| TableRecord {
            tag: [chunk[0], chunk[1], chunk[2], chunk[3]],
            offset: u32::from_be_bytes([chunk[8], chunk[9], chunk[10], chunk[11]]) as usize,
            length: u32::from_be_bytes([chunk[12], chunk[13], chunk[14], chunk[15]]) as usize,
        })
        .collect();

    Ok(TableDirectory { tables })
}

fn parse_head(data: &[u8]) -> Result<HeadTable, OpenTypeError> {
    if data.len() < HEAD_TABLE_LEN {
        return Err(OpenTypeError::Truncated);
    }

    let magic = read_u32(data, 12)?;
    if magic != HEAD_MAGIC {
        return Err(invalid_table_data("head", "missing expected magic number"));
    }

    let units_per_em = read_u16(data, 18)?;
    if units_per_em == 0 {
        return Err(invalid_table_data("head", "unitsPerEm must be non-zero"));
    }

    let index_to_loc_format = read_i16(data, 50)?;
    if !matches!(index_to_loc_format, 0 | 1) {
        return Err(invalid_table_data(
            "head",
            format!("unsupported indexToLocFormat: {index_to_loc_format}"),
        ));
    }

    Ok(HeadTable {
        flags: read_u16(data, 16)?,
        units_per_em,
        index_to_loc_format,
        x_min: read_i16(data, 36)?,
        y_min: read_i16(data, 38)?,
        x_max: read_i16(data, 40)?,
        y_max: read_i16(data, 42)?,
    })
}

fn parse_name_table(table: &[u8]) -> Result<Vec<String>, OpenTypeError> {
    if table.len() < NAME_HEADER_LEN {
        return Err(OpenTypeError::Truncated);
    }

    let format = read_u16(table, 0)?;
    if !matches!(format, 0 | 1) {
        return Err(invalid_table_data(
            "name",
            format!("unsupported name table format: {format}"),
        ));
    }

    let count = usize::from(read_u16(table, 2)?);
    let string_offset = usize::from(read_u16(table, 4)?);
    let records = read_slice(
        table,
        NAME_HEADER_LEN,
        count
            .checked_mul(NAME_RECORD_LEN)
            .ok_or(OpenTypeError::Truncated)?,
    )?;
    let mut names = BTreeSet::new();

    for record in records.chunks_exact(NAME_RECORD_LEN) {
        let platform_id = u16::from_be_bytes([record[0], record[1]]);
        let encoding_id = u16::from_be_bytes([record[2], record[3]]);
        let name_id = u16::from_be_bytes([record[6], record[7]]);
        if !matches!(name_id, 1 | 4 | 16) {
            continue;
        }

        let length = usize::from(u16::from_be_bytes([record[8], record[9]]));
        let offset = usize::from(u16::from_be_bytes([record[10], record[11]]));
        let raw = match read_slice(table, string_offset.saturating_add(offset), length) {
            Ok(raw) => raw,
            Err(_) => continue,
        };
        let Some(name) = decode_name_record(raw, platform_id, encoding_id) else {
            continue;
        };
        let trimmed = name.trim_matches('\0').trim();
        if !trimmed.is_empty() {
            names.insert(trimmed.to_string());
        }
    }

    Ok(names.into_iter().collect())
}

fn decode_name_record(raw: &[u8], platform_id: u16, _encoding_id: u16) -> Option<String> {
    match platform_id {
        0 | 3 => decode_utf16be(raw),
        1 => Some(String::from_utf8_lossy(raw).into_owned()),
        _ => std::str::from_utf8(raw).ok().map(ToOwned::to_owned),
    }
}

fn decode_utf16be(raw: &[u8]) -> Option<String> {
    if raw.len() % 2 != 0 {
        return None;
    }

    let code_units = raw
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect::<Vec<_>>();

    String::from_utf16(&code_units).ok()
}

fn parse_maxp(data: &[u8]) -> Result<MaxpTable, OpenTypeError> {
    if data.len() < MAXP_TABLE_MIN_LEN {
        return Err(OpenTypeError::Truncated);
    }

    let num_glyphs = read_u16(data, 4)?;
    if num_glyphs == 0 {
        return Err(invalid_table_data("maxp", "numGlyphs must be non-zero"));
    }

    Ok(MaxpTable { num_glyphs })
}

fn parse_hhea(data: &[u8]) -> Result<HheaTable, OpenTypeError> {
    if data.len() < HHEA_TABLE_LEN {
        return Err(OpenTypeError::Truncated);
    }

    let number_of_h_metrics = read_u16(data, 34)?;
    if number_of_h_metrics == 0 {
        return Err(invalid_table_data(
            "hhea",
            "numberOfHMetrics must be at least 1",
        ));
    }

    Ok(HheaTable {
        ascender: read_i16(data, 4)?,
        descender: read_i16(data, 6)?,
        line_gap: read_i16(data, 8)?,
        number_of_h_metrics,
    })
}

fn parse_hmtx(
    data: &[u8],
    number_of_h_metrics: u16,
    num_glyphs: u16,
) -> Result<HmtxTable, OpenTypeError> {
    let metric_count = usize::from(number_of_h_metrics);
    if metric_count == 0 {
        return Err(invalid_table_data(
            "hmtx",
            "numberOfHMetrics must be at least 1",
        ));
    }
    if number_of_h_metrics > num_glyphs {
        return Err(invalid_table_data(
            "hmtx",
            "numberOfHMetrics must not exceed numGlyphs",
        ));
    }

    let metrics_len = metric_count
        .checked_mul(4)
        .ok_or_else(|| invalid_table_data("hmtx", "horizontal metrics length overflow"))?;
    let metrics_bytes = read_slice(data, 0, metrics_len)?;
    let h_metrics = metrics_bytes
        .chunks_exact(4)
        .map(|chunk| HorizontalMetric {
            advance_width: u16::from_be_bytes([chunk[0], chunk[1]]),
            lsb: i16::from_be_bytes([chunk[2], chunk[3]]),
        })
        .collect();

    let trailing_bytes = data.get(metrics_len..).ok_or(OpenTypeError::Truncated)?;
    let expected_lsb_count = usize::from(num_glyphs - number_of_h_metrics);
    if trailing_bytes.len() != expected_lsb_count * 2 {
        return Err(invalid_table_data(
            "hmtx",
            format!(
                "left side bearing count mismatch: expected {}, got {}",
                expected_lsb_count,
                trailing_bytes.len() / 2
            ),
        ));
    }
    if trailing_bytes.len() % 2 != 0 {
        return Err(invalid_table_data(
            "hmtx",
            "left side bearings must be 2-byte aligned",
        ));
    }

    let left_side_bearings = trailing_bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_be_bytes([chunk[0], chunk[1]]))
        .collect();

    Ok(HmtxTable {
        h_metrics,
        left_side_bearings,
    })
}

fn parse_glyph_table(
    table_directory: &TableDirectory,
    data: &[u8],
    num_glyphs: u16,
    index_to_loc_format: i16,
) -> Result<Option<GlyphTable>, OpenTypeError> {
    let Some(loca_record) = table_directory.maybe_table_record("loca").cloned() else {
        return Ok(None);
    };
    let Some(glyf_record) = table_directory.maybe_table_record("glyf").cloned() else {
        return Ok(None);
    };

    let loca_data = read_slice(data, loca_record.offset, loca_record.length)?;
    let glyf_data = read_slice(data, glyf_record.offset, glyf_record.length)?;
    let loca_offsets = parse_loca(loca_data, num_glyphs, index_to_loc_format, glyf_data.len())?;

    Ok(Some(GlyphTable {
        loca_record,
        glyf_record,
        loca_offsets,
    }))
}

fn parse_loca(
    data: &[u8],
    num_glyphs: u16,
    index_to_loc_format: i16,
    glyf_len: usize,
) -> Result<Vec<u32>, OpenTypeError> {
    let entry_count = usize::from(num_glyphs) + 1;
    let mut offsets = Vec::with_capacity(entry_count);
    match index_to_loc_format {
        0 => {
            let entries = read_u16_vec(data, 0, entry_count)?;
            offsets.extend(entries.into_iter().map(|value| u32::from(value) * 2));
        }
        1 => {
            let bytes = read_slice(data, 0, entry_count * 4)?;
            offsets.extend(
                bytes
                    .chunks_exact(4)
                    .map(|chunk| u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])),
            );
        }
        _ => {
            return Err(invalid_table_data(
                "loca",
                format!("unsupported indexToLocFormat: {index_to_loc_format}"),
            ));
        }
    }

    for window in offsets.windows(2) {
        if window[0] > window[1] {
            return Err(invalid_table_data(
                "loca",
                "glyph offsets must be non-decreasing",
            ));
        }
    }
    if usize::try_from(*offsets.last().unwrap_or(&0))
        .ok()
        .unwrap_or(usize::MAX)
        > glyf_len
    {
        return Err(invalid_table_data(
            "loca",
            "glyph offsets point outside the glyf table",
        ));
    }

    Ok(offsets)
}

fn parse_cmap(data: &[u8]) -> Result<CmapTable, OpenTypeError> {
    let num_tables = usize::from(read_u16(data, 2)?);
    let records = read_slice(
        data,
        CMAP_HEADER_LEN,
        num_tables
            .checked_mul(CMAP_ENCODING_RECORD_LEN)
            .ok_or(OpenTypeError::Truncated)?,
    )?;

    let mut best_candidate = None;
    for chunk in records.chunks_exact(CMAP_ENCODING_RECORD_LEN) {
        let platform_id = u16::from_be_bytes([chunk[0], chunk[1]]);
        let encoding_id = u16::from_be_bytes([chunk[2], chunk[3]]);
        let offset = u32::from_be_bytes([chunk[4], chunk[5], chunk[6], chunk[7]]) as usize;

        let priority = match (platform_id, encoding_id) {
            (3, 1) => 0usize,
            (0, _) => 1usize,
            _ => continue,
        };

        let format = read_u16(data, offset)?;
        if format != 4 {
            continue;
        }

        match best_candidate {
            Some((best_priority, _)) if best_priority <= priority => {}
            _ => best_candidate = Some((priority, offset)),
        }
    }

    let (_, offset) = best_candidate
        .ok_or_else(|| invalid_table_data("cmap", "missing supported Unicode format 4 subtable"))?;
    let subtable = read_slice(data, offset, data.len().saturating_sub(offset))?;
    parse_cmap_format4(subtable)
}

fn parse_cmap_format4(data: &[u8]) -> Result<CmapTable, OpenTypeError> {
    if read_u16(data, 0)? != 4 {
        return Err(invalid_table_data(
            "cmap",
            "unsupported cmap subtable format",
        ));
    }

    let length = usize::from(read_u16(data, 2)?);
    let table = read_slice(data, 0, length)?;
    let seg_count_x2 = read_u16(table, 6)?;
    if seg_count_x2 == 0 || seg_count_x2 % 2 != 0 {
        return Err(invalid_table_data(
            "cmap",
            "segCountX2 must be even and non-zero",
        ));
    }
    let seg_count = usize::from(seg_count_x2 / 2);

    let end_codes_offset = CMAP_FORMAT_4_HEADER_LEN;
    let reserved_pad_offset = end_codes_offset + seg_count * 2;
    let start_codes_offset = reserved_pad_offset + 2;
    let id_delta_offset = start_codes_offset + seg_count * 2;
    let id_range_offset_offset = id_delta_offset + seg_count * 2;
    let glyph_id_array_offset = id_range_offset_offset + seg_count * 2;

    if read_u16(table, reserved_pad_offset)? != 0 {
        return Err(invalid_table_data("cmap", "reservedPad must be zero"));
    }

    let end_codes = read_u16_vec(table, end_codes_offset, seg_count)?;
    let start_codes = read_u16_vec(table, start_codes_offset, seg_count)?;
    let id_deltas = read_i16_vec(table, id_delta_offset, seg_count)?;
    let id_range_offsets = read_u16_vec(table, id_range_offset_offset, seg_count)?;

    if glyph_id_array_offset > table.len() {
        return Err(OpenTypeError::Truncated);
    }
    if (table.len() - glyph_id_array_offset) % 2 != 0 {
        return Err(invalid_table_data(
            "cmap",
            "glyphIdArray must be 2-byte aligned",
        ));
    }
    let glyph_id_array = read_u16_vec(
        table,
        glyph_id_array_offset,
        (table.len() - glyph_id_array_offset) / 2,
    )?;

    let mut segments = Vec::with_capacity(seg_count);
    for index in 0..seg_count {
        let start_code = start_codes[index];
        let end_code = end_codes[index];
        if start_code > end_code {
            return Err(invalid_table_data(
                "cmap",
                format!("segment {index} has startCode greater than endCode"),
            ));
        }

        let id_range_offset = id_range_offsets[index];
        if id_range_offset % 2 != 0 {
            return Err(invalid_table_data(
                "cmap",
                format!("segment {index} has odd idRangeOffset"),
            ));
        }

        if id_range_offset != 0 {
            let base_index = index + usize::from(id_range_offset / 2);
            if base_index < seg_count {
                return Err(invalid_table_data(
                    "cmap",
                    format!("segment {index} points before glyphIdArray"),
                ));
            }
            let first_index = base_index - seg_count;
            let last_index = first_index + usize::from(end_code - start_code);
            if last_index >= glyph_id_array.len() {
                return Err(invalid_table_data(
                    "cmap",
                    format!("segment {index} points outside glyphIdArray"),
                ));
            }
        }

        segments.push(CmapFormat4Segment {
            start_code,
            end_code,
            id_delta: id_deltas[index],
            id_range_offset,
        });
    }

    Ok(CmapTable {
        segments,
        glyph_id_array,
    })
}

fn read_u16_vec(data: &[u8], offset: usize, count: usize) -> Result<Vec<u16>, OpenTypeError> {
    let bytes = read_slice(data, offset, count * 2)?;
    Ok(bytes
        .chunks_exact(2)
        .map(|chunk| u16::from_be_bytes([chunk[0], chunk[1]]))
        .collect())
}

fn read_i16_vec(data: &[u8], offset: usize, count: usize) -> Result<Vec<i16>, OpenTypeError> {
    let bytes = read_slice(data, offset, count * 2)?;
    Ok(bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_be_bytes([chunk[0], chunk[1]]))
        .collect())
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, OpenTypeError> {
    let bytes = read_slice(data, offset, 2)?;
    Ok(u16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_i16(data: &[u8], offset: usize) -> Result<i16, OpenTypeError> {
    let bytes = read_slice(data, offset, 2)?;
    Ok(i16::from_be_bytes([bytes[0], bytes[1]]))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, OpenTypeError> {
    let bytes = read_slice(data, offset, 4)?;
    Ok(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn read_slice(data: &[u8], offset: usize, len: usize) -> Result<&[u8], OpenTypeError> {
    let end = offset.saturating_add(len);
    data.get(offset..end).ok_or(OpenTypeError::Truncated)
}

fn tag_to_bytes(tag: &str) -> [u8; 4] {
    let bytes = tag.as_bytes();
    [bytes[0], bytes[1], bytes[2], bytes[3]]
}

fn invalid_table_data(table: &'static str, description: impl Into<String>) -> OpenTypeError {
    OpenTypeError::InvalidTableData {
        table,
        description: description.into(),
    }
}

fn build_subset_glyf(
    glyf_data: &[u8],
    loca_offsets: &[u32],
    used_glyph_ids: &BTreeSet<u16>,
) -> Result<(Vec<u8>, Vec<u32>), OpenTypeError> {
    let mut retained_glyphs = used_glyph_ids.clone();
    retained_glyphs.insert(0);

    let mut subset_glyf = Vec::with_capacity(glyf_data.len());
    let mut subset_loca_offsets = Vec::with_capacity(loca_offsets.len());

    for glyph_index in 0..loca_offsets.len().saturating_sub(1) {
        subset_loca_offsets
            .push(u32::try_from(subset_glyf.len()).expect("subset glyf length must fit in u32"));

        if !retained_glyphs
            .contains(&u16::try_from(glyph_index).expect("glyph index must fit in u16"))
        {
            continue;
        }

        let start = usize::try_from(loca_offsets[glyph_index])
            .map_err(|_| invalid_table_data("loca", "glyph offset overflow"))?;
        let end = usize::try_from(loca_offsets[glyph_index + 1])
            .map_err(|_| invalid_table_data("loca", "glyph offset overflow"))?;
        subset_glyf.extend_from_slice(read_slice(glyf_data, start, end.saturating_sub(start))?);
        if subset_glyf.len() % 2 != 0 {
            subset_glyf.push(0);
        }
    }

    subset_loca_offsets
        .push(u32::try_from(subset_glyf.len()).expect("subset glyf length must fit in u32"));

    Ok((subset_glyf, subset_loca_offsets))
}

fn build_loca_table(
    loca_offsets: &[u32],
    index_to_loc_format: i16,
) -> Result<Vec<u8>, OpenTypeError> {
    match index_to_loc_format {
        0 => {
            let mut data = Vec::with_capacity(loca_offsets.len() * 2);
            for offset in loca_offsets {
                if offset % 2 != 0 {
                    return Err(invalid_table_data(
                        "loca",
                        "short loca offsets must be even",
                    ));
                }
                let short_offset = u16::try_from(offset / 2).map_err(|_| {
                    invalid_table_data("loca", "short loca offset exceeds u16 range")
                })?;
                data.extend_from_slice(&short_offset.to_be_bytes());
            }
            Ok(data)
        }
        1 => {
            let mut data = Vec::with_capacity(loca_offsets.len() * 4);
            for offset in loca_offsets {
                data.extend_from_slice(&offset.to_be_bytes());
            }
            Ok(data)
        }
        _ => Err(invalid_table_data(
            "loca",
            format!("unsupported indexToLocFormat: {index_to_loc_format}"),
        )),
    }
}

fn build_sfnt(sf_version: u32, tables: &[([u8; 4], Vec<u8>)]) -> Vec<u8> {
    let directory_len = SFNT_HEADER_LEN + tables.len() * TABLE_RECORD_LEN;
    let mut offsets = Vec::with_capacity(tables.len());
    let mut next_offset = directory_len;
    for (_, table_data) in tables {
        next_offset = align_to_four(next_offset);
        offsets.push(next_offset);
        next_offset += align_to_four(table_data.len());
    }

    let mut data = Vec::with_capacity(next_offset);
    data.extend_from_slice(&sf_version.to_be_bytes());
    data.extend_from_slice(&(u16::try_from(tables.len()).expect("table count")).to_be_bytes());
    data.extend_from_slice(&0u16.to_be_bytes());
    data.extend_from_slice(&0u16.to_be_bytes());
    data.extend_from_slice(&0u16.to_be_bytes());

    for ((tag, table_data), offset) in tables.iter().zip(offsets.iter()) {
        data.extend_from_slice(tag);
        data.extend_from_slice(&table_checksum(table_data).to_be_bytes());
        data.extend_from_slice(&u32::try_from(*offset).expect("table offset").to_be_bytes());
        data.extend_from_slice(
            &u32::try_from(table_data.len())
                .expect("table length")
                .to_be_bytes(),
        );
    }

    let mut current_offset = directory_len;
    for ((_, table_data), offset) in tables.iter().zip(offsets.iter()) {
        while current_offset < *offset {
            data.push(0);
            current_offset += 1;
        }
        data.extend_from_slice(table_data);
        current_offset += table_data.len();
        while current_offset % 4 != 0 {
            data.push(0);
            current_offset += 1;
        }
    }

    data
}

fn table_checksum(data: &[u8]) -> u32 {
    let mut sum = 0u32;
    let padded_len = align_to_four(data.len());
    let mut buffer = vec![0u8; padded_len];
    buffer[..data.len()].copy_from_slice(data);
    for chunk in buffer.chunks_exact(4) {
        sum = sum.wrapping_add(u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    sum
}

fn align_to_four(value: usize) -> usize {
    (value + 3) & !3
}

#[cfg(test)]
fn write_u16(data: &mut [u8], offset: usize, value: u16) {
    data[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
fn write_i16(data: &mut [u8], offset: usize, value: i16) {
    data[offset..offset + 2].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
fn write_u32(data: &mut [u8], offset: usize, value: u32) {
    data[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use crate::kernel::api::DimensionValue;
    use crate::typesetting::api::CharWidthProvider;

    use crate::font::api::OpenTypeWidthProvider;

    use super::{
        build_loca_table, build_sfnt, extract_font_names, parse_cmap, write_i16, write_u16,
        write_u32, OpenTypeError, OpenTypeFont, HEAD_MAGIC, OPENTYPE_CFF_MAGIC, TRUETYPE_MAGIC,
    };

    const SCALED_POINTS_PER_POINT: i64 = 65_536;

    #[test]
    fn parses_minimal_ttf_with_head_hhea_hmtx_cmap() {
        let data = build_test_font(TestFont {
            sf_version: TRUETYPE_MAGIC,
            units_per_em: 1000,
            flags: 0x0005,
            index_to_loc_format: 1,
            ascender: 800,
            descender: -200,
            line_gap: 200,
            h_metrics: &[(500, 0), (600, 10)],
            extra_lsbs: &[20],
            cmap_segments: &[TestCmapSegment {
                start_code: 65,
                end_code: 66,
                id_delta: 0,
                glyph_ids: &[1, 2],
            }],
            font_names: &[],
        });

        let font = OpenTypeFont::parse(&data).expect("parse minimal font");

        assert_eq!(font.glyph_id(65), Some(1));
        assert_eq!(font.glyph_id(66), Some(2));
        assert_eq!(font.advance_width(1), Some(600));
        assert_eq!(font.advance_width(2), Some(600));
        assert_eq!(font.units_per_em(), 1000);
        assert_eq!(font.ascender(), 800);
        assert_eq!(font.descender(), -200);
        assert_eq!(font.line_gap(), 200);
        assert_eq!(font.bounding_box(), [-50, -200, 1000, 800]);
        assert_eq!(font.raw_data(), data.as_slice());
    }

    #[test]
    fn returns_error_for_truncated_data() {
        let error = OpenTypeFont::parse(&[0; 8]).expect_err("short data should fail");

        assert_eq!(error, OpenTypeError::Truncated);
    }

    #[test]
    fn returns_error_for_invalid_magic() {
        let mut data = vec![0; 12];
        data[0..4].copy_from_slice(&0x1234_5678u32.to_be_bytes());

        let error = OpenTypeFont::parse(&data).expect_err("invalid magic should fail");

        assert_eq!(error, OpenTypeError::InvalidMagic);
    }

    #[test]
    fn returns_none_for_unmapped_codepoint() {
        let data = build_test_font(TestFont {
            sf_version: OPENTYPE_CFF_MAGIC,
            units_per_em: 1000,
            flags: 0,
            index_to_loc_format: 0,
            ascender: 700,
            descender: -150,
            line_gap: 100,
            h_metrics: &[(500, 0), (600, 10)],
            extra_lsbs: &[20],
            cmap_segments: &[TestCmapSegment {
                start_code: 65,
                end_code: 66,
                id_delta: 0,
                glyph_ids: &[1, 2],
            }],
            font_names: &[],
        });
        let font = OpenTypeFont::parse(&data).expect("parse font");

        assert_eq!(font.glyph_id(67), None);
    }

    #[test]
    fn handles_cmap_format4_id_delta_mapping() {
        let cmap = build_cmap_table(
            3,
            1,
            &[TestCmapSegment {
                start_code: 97,
                end_code: 98,
                id_delta: -93,
                glyph_ids: &[],
            }],
        );

        let table = parse_cmap(&cmap).expect("parse cmap");

        assert_eq!(table.lookup(97), Some(4));
        assert_eq!(table.lookup(98), Some(5));
        assert_eq!(table.lookup(99), None);
    }

    #[test]
    fn opentype_width_provider_scales_character_widths() {
        let data = build_test_font(TestFont {
            sf_version: TRUETYPE_MAGIC,
            units_per_em: 1000,
            flags: 0,
            index_to_loc_format: 1,
            ascender: 800,
            descender: -200,
            line_gap: 200,
            h_metrics: &[(400, 0), (250, 0), (500, 0)],
            extra_lsbs: &[],
            cmap_segments: &[
                TestCmapSegment {
                    start_code: 32,
                    end_code: 32,
                    id_delta: 0,
                    glyph_ids: &[1],
                },
                TestCmapSegment {
                    start_code: 65,
                    end_code: 65,
                    id_delta: 0,
                    glyph_ids: &[2],
                },
            ],
            font_names: &[],
        });
        let font = OpenTypeFont::parse(&data).expect("parse font");
        let provider = OpenTypeWidthProvider {
            font: &font,
            fallback_width: DimensionValue(SCALED_POINTS_PER_POINT),
        };

        assert_eq!(provider.char_width('A'), DimensionValue(32_768));
        assert_eq!(provider.space_width(), DimensionValue(16_384));
        assert_eq!(
            provider.char_width('Z'),
            DimensionValue(SCALED_POINTS_PER_POINT)
        );
    }

    #[test]
    fn subsets_glyph_data_by_zeroing_unused_entries() {
        let data = build_test_font(TestFont {
            sf_version: TRUETYPE_MAGIC,
            units_per_em: 1000,
            flags: 0,
            index_to_loc_format: 1,
            ascender: 800,
            descender: -200,
            line_gap: 200,
            h_metrics: &[(400, 0), (500, 0), (600, 0)],
            extra_lsbs: &[],
            cmap_segments: &[TestCmapSegment {
                start_code: 65,
                end_code: 66,
                id_delta: 0,
                glyph_ids: &[1, 2],
            }],
            font_names: &[],
        });
        let font = OpenTypeFont::parse(&data).expect("parse font");
        let mut used_glyph_ids = BTreeSet::new();
        used_glyph_ids.insert(2);

        let subset = font.subset(&used_glyph_ids);
        let subset_font = OpenTypeFont::parse(&subset).expect("parse subset");
        let glyph_table = subset_font
            .glyph_table
            .as_ref()
            .expect("subset glyph table should exist");

        assert!(subset.len() < data.len());
        assert_eq!(glyph_table.loca_offsets, vec![0, 10, 10, 20]);
    }

    #[test]
    fn extracts_family_and_full_font_names_from_name_table() {
        let data = build_test_font(TestFont {
            sf_version: TRUETYPE_MAGIC,
            units_per_em: 1000,
            flags: 0x0005,
            index_to_loc_format: 1,
            ascender: 800,
            descender: -200,
            line_gap: 200,
            h_metrics: &[(500, 0), (600, 10)],
            extra_lsbs: &[20],
            cmap_segments: &[TestCmapSegment {
                start_code: 65,
                end_code: 66,
                id_delta: 0,
                glyph_ids: &[1, 2],
            }],
            font_names: &[
                TestFontName {
                    name_id: 1,
                    value: "Noto Serif",
                },
                TestFontName {
                    name_id: 4,
                    value: "Noto Serif Regular",
                },
            ],
        });

        let names = extract_font_names(&data).expect("extract font names");

        assert_eq!(
            names,
            vec!["Noto Serif".to_string(), "Noto Serif Regular".to_string()]
        );
    }

    struct TestFont<'a> {
        sf_version: u32,
        units_per_em: u16,
        flags: u16,
        index_to_loc_format: i16,
        ascender: i16,
        descender: i16,
        line_gap: i16,
        h_metrics: &'a [(u16, i16)],
        extra_lsbs: &'a [i16],
        cmap_segments: &'a [TestCmapSegment<'a>],
        font_names: &'a [TestFontName<'a>],
    }

    struct TestCmapSegment<'a> {
        start_code: u16,
        end_code: u16,
        id_delta: i16,
        glyph_ids: &'a [u16],
    }

    struct TestFontName<'a> {
        name_id: u16,
        value: &'a str,
    }

    pub(crate) fn minimal_test_font_bytes() -> Vec<u8> {
        build_test_font(TestFont {
            sf_version: TRUETYPE_MAGIC,
            units_per_em: 1000,
            flags: 0x0005,
            index_to_loc_format: 1,
            ascender: 800,
            descender: -200,
            line_gap: 200,
            h_metrics: &[(500, 0), (600, 10)],
            extra_lsbs: &[20],
            cmap_segments: &[TestCmapSegment {
                start_code: 65,
                end_code: 66,
                id_delta: 0,
                glyph_ids: &[1, 2],
            }],
            font_names: &[],
        })
    }

    pub(crate) fn named_test_font_bytes(family_name: &str, full_font_name: &str) -> Vec<u8> {
        build_test_font(TestFont {
            sf_version: TRUETYPE_MAGIC,
            units_per_em: 1000,
            flags: 0x0005,
            index_to_loc_format: 1,
            ascender: 800,
            descender: -200,
            line_gap: 200,
            h_metrics: &[(500, 0), (600, 10)],
            extra_lsbs: &[20],
            cmap_segments: &[TestCmapSegment {
                start_code: 65,
                end_code: 66,
                id_delta: 0,
                glyph_ids: &[1, 2],
            }],
            font_names: &[
                TestFontName {
                    name_id: 1,
                    value: family_name,
                },
                TestFontName {
                    name_id: 4,
                    value: full_font_name,
                },
            ],
        })
    }

    fn build_test_font(config: TestFont<'_>) -> Vec<u8> {
        let glyph_count = config.h_metrics.len() + config.extra_lsbs.len();
        let head = build_head_table(
            config.units_per_em,
            config.flags,
            config.index_to_loc_format,
        );
        let hhea = build_hhea_table(
            config.ascender,
            config.descender,
            config.line_gap,
            u16::try_from(config.h_metrics.len()).expect("h_metrics length"),
        );
        let maxp = build_maxp_table(u16::try_from(glyph_count).expect("glyph count"));
        let hmtx = build_hmtx_table(config.h_metrics, config.extra_lsbs);
        let cmap = build_cmap_table(3, 1, config.cmap_segments);
        let glyphs = build_default_glyphs(glyph_count);
        let (loca, glyf) = build_glyph_tables(&glyphs, config.index_to_loc_format);
        let name = (!config.font_names.is_empty()).then(|| build_name_table(config.font_names));

        let mut tables = vec![
            (*b"head", head),
            (*b"hhea", hhea),
            (*b"maxp", maxp),
            (*b"hmtx", hmtx),
            (*b"cmap", cmap),
            (*b"loca", loca),
            (*b"glyf", glyf),
        ];
        if let Some(name) = name {
            tables.push((*b"name", name));
        }

        build_sfnt(config.sf_version, &tables)
    }

    fn build_name_table(names: &[TestFontName<'_>]) -> Vec<u8> {
        let mut string_storage = Vec::new();
        let mut records = Vec::with_capacity(names.len());

        for name in names {
            let encoded = name
                .value
                .encode_utf16()
                .flat_map(u16::to_be_bytes)
                .collect::<Vec<_>>();
            let offset = u16::try_from(string_storage.len()).expect("name string offset");
            let length = u16::try_from(encoded.len()).expect("name string length");
            string_storage.extend_from_slice(&encoded);
            records.push((name.name_id, length, offset));
        }

        let name_header_len = 6usize;
        let name_record_len = 12usize;
        let mut data = Vec::with_capacity(name_header_len + records.len() * name_record_len);
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(
            &(u16::try_from(records.len()).expect("name record count")).to_be_bytes(),
        );
        data.extend_from_slice(
            &(u16::try_from(name_header_len + records.len() * name_record_len)
                .expect("name string storage offset"))
            .to_be_bytes(),
        );

        for (name_id, length, offset) in records {
            data.extend_from_slice(&3u16.to_be_bytes());
            data.extend_from_slice(&1u16.to_be_bytes());
            data.extend_from_slice(&0x0409u16.to_be_bytes());
            data.extend_from_slice(&name_id.to_be_bytes());
            data.extend_from_slice(&length.to_be_bytes());
            data.extend_from_slice(&offset.to_be_bytes());
        }

        data.extend_from_slice(&string_storage);
        data
    }

    fn build_head_table(units_per_em: u16, flags: u16, index_to_loc_format: i16) -> Vec<u8> {
        let mut data = vec![0; 54];
        write_u32(&mut data, 0, 0x0001_0000);
        write_u32(&mut data, 12, HEAD_MAGIC);
        write_u16(&mut data, 16, flags);
        write_u16(&mut data, 18, units_per_em);
        write_i16(&mut data, 36, -50);
        write_i16(&mut data, 38, -200);
        write_i16(&mut data, 40, 1000);
        write_i16(&mut data, 42, 800);
        write_i16(&mut data, 50, index_to_loc_format);
        data
    }

    fn build_maxp_table(num_glyphs: u16) -> Vec<u8> {
        let mut data = vec![0; 6];
        write_u32(&mut data, 0, 0x0001_0000);
        write_u16(&mut data, 4, num_glyphs);
        data
    }

    fn build_hhea_table(
        ascender: i16,
        descender: i16,
        line_gap: i16,
        number_of_h_metrics: u16,
    ) -> Vec<u8> {
        let mut data = vec![0; 36];
        write_u32(&mut data, 0, 0x0001_0000);
        write_i16(&mut data, 4, ascender);
        write_i16(&mut data, 6, descender);
        write_i16(&mut data, 8, line_gap);
        write_u16(&mut data, 34, number_of_h_metrics);
        data
    }

    fn build_hmtx_table(h_metrics: &[(u16, i16)], extra_lsbs: &[i16]) -> Vec<u8> {
        let mut data = Vec::with_capacity(h_metrics.len() * 4 + extra_lsbs.len() * 2);
        for (advance_width, lsb) in h_metrics {
            data.extend_from_slice(&advance_width.to_be_bytes());
            data.extend_from_slice(&lsb.to_be_bytes());
        }
        for lsb in extra_lsbs {
            data.extend_from_slice(&lsb.to_be_bytes());
        }
        data
    }

    fn build_default_glyphs(count: usize) -> Vec<Vec<u8>> {
        (0..count)
            .map(|index| {
                let mut glyph = vec![0; 10];
                write_i16(&mut glyph, 0, if index == 0 { 0 } else { 1 });
                write_i16(&mut glyph, 2, 0);
                write_i16(&mut glyph, 4, 0);
                write_i16(&mut glyph, 6, 50 + index as i16);
                write_i16(&mut glyph, 8, 100 + index as i16);
                glyph
            })
            .collect()
    }

    fn build_glyph_tables(glyphs: &[Vec<u8>], index_to_loc_format: i16) -> (Vec<u8>, Vec<u8>) {
        let mut glyf = Vec::new();
        let mut offsets = Vec::with_capacity(glyphs.len() + 1);

        for glyph in glyphs {
            offsets.push(u32::try_from(glyf.len()).expect("glyf offset"));
            glyf.extend_from_slice(glyph);
            if glyf.len() % 2 != 0 {
                glyf.push(0);
            }
        }
        offsets.push(u32::try_from(glyf.len()).expect("glyf offset"));

        (
            build_loca_table(&offsets, index_to_loc_format).expect("build loca"),
            glyf,
        )
    }

    fn build_cmap_table(
        platform_id: u16,
        encoding_id: u16,
        segments: &[TestCmapSegment<'_>],
    ) -> Vec<u8> {
        let format4 = build_cmap_format4(segments);

        let mut data = Vec::with_capacity(12 + format4.len());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&1u16.to_be_bytes());
        data.extend_from_slice(&platform_id.to_be_bytes());
        data.extend_from_slice(&encoding_id.to_be_bytes());
        data.extend_from_slice(&12u32.to_be_bytes());
        data.extend_from_slice(&format4);
        data
    }

    fn build_cmap_format4(segments: &[TestCmapSegment<'_>]) -> Vec<u8> {
        let mut all_segments = segments
            .iter()
            .map(|segment| TestCmapSegment {
                start_code: segment.start_code,
                end_code: segment.end_code,
                id_delta: segment.id_delta,
                glyph_ids: segment.glyph_ids,
            })
            .collect::<Vec<_>>();
        all_segments.push(TestCmapSegment {
            start_code: 0xffff,
            end_code: 0xffff,
            id_delta: 1,
            glyph_ids: &[],
        });

        let seg_count = all_segments.len();
        let mut end_codes = Vec::with_capacity(seg_count);
        let mut start_codes = Vec::with_capacity(seg_count);
        let mut id_deltas = Vec::with_capacity(seg_count);
        let mut id_range_offsets = Vec::with_capacity(seg_count);
        let mut glyph_id_array = Vec::new();

        for (index, segment) in all_segments.iter().enumerate() {
            assert!(segment.start_code <= segment.end_code);
            if segment.glyph_ids.is_empty() {
                id_range_offsets.push(0u16);
            } else {
                assert_eq!(
                    segment.glyph_ids.len(),
                    usize::from(segment.end_code - segment.start_code + 1),
                );
                let offset_words = seg_count - index + glyph_id_array.len();
                id_range_offsets.push(u16::try_from(offset_words * 2).expect("idRangeOffset"));
                glyph_id_array.extend_from_slice(segment.glyph_ids);
            }
            end_codes.push(segment.end_code);
            start_codes.push(segment.start_code);
            id_deltas.push(segment.id_delta);
        }

        let seg_count_x2 = u16::try_from(seg_count * 2).expect("seg_count_x2");
        let length = 16 + seg_count * 8 + glyph_id_array.len() * 2;
        let mut data = Vec::with_capacity(length);
        data.extend_from_slice(&4u16.to_be_bytes());
        data.extend_from_slice(&u16::try_from(length).expect("format4 length").to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&seg_count_x2.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes());

        for value in end_codes {
            data.extend_from_slice(&value.to_be_bytes());
        }
        data.extend_from_slice(&0u16.to_be_bytes());
        for value in start_codes {
            data.extend_from_slice(&value.to_be_bytes());
        }
        for value in id_deltas {
            data.extend_from_slice(&value.to_be_bytes());
        }
        for value in id_range_offsets {
            data.extend_from_slice(&value.to_be_bytes());
        }
        for value in glyph_id_array {
            data.extend_from_slice(&value.to_be_bytes());
        }

        data
    }
}

#[cfg(test)]
pub(crate) use tests::{minimal_test_font_bytes, named_test_font_bytes};
