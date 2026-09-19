//! 16×16 点阵汉字与字符字模检索及半角像素块渲染。
//!
//! 内置预生成 16×16 1-bit 点阵字库（Noto Sans CJK 光栅化产物），
//! 涵盖全部常用单字赛文与一击字单字，每个字模占 32 字节。
//! 终端使用半角 Unicode 字符（`▀`、`▄`、`█`、` `）渲染，每个字占 16 列 × 8 行。

static FONT_DATA: &[u8] = include_bytes!("../data/font16.bin");

const HEADER_MAGIC: &[u8; 4] = b"DT16";
const ENTRY_SIZE: usize = 4 + 32; // u32 codepoint + 32 bytes bitmap

/// 默认回退方框字模（16×16 矩形方框 □）。
pub static FALLBACK_GLYPH: [u8; 32] = [
    0x00, 0x00, // 0
    0x00, 0x00, // 1
    0x3F, 0xFC, // 2  ######
    0x20, 0x04, // 3  #    #
    0x20, 0x04, // 4  #    #
    0x20, 0x04, // 5  #    #
    0x20, 0x04, // 6  #    #
    0x20, 0x04, // 7  #    #
    0x20, 0x04, // 8  #    #
    0x20, 0x04, // 9  #    #
    0x20, 0x04, // 10 #    #
    0x20, 0x04, // 11 #    #
    0x20, 0x04, // 12 #    #
    0x3F, 0xFC, // 13 ######
    0x00, 0x00, // 14
    0x00, 0x00, // 15
];

/// 查找字符对应的 16×16 位图（32 字节，16 行 × 2 字节，高位在左）。
pub fn glyph_for_char(c: char) -> Option<&'static [u8; 32]> {
    if FONT_DATA.len() < 8 || &FONT_DATA[..4] != HEADER_MAGIC {
        return None;
    }
    let num_entries = u32::from_le_bytes(FONT_DATA[4..8].try_into().ok()?) as usize;
    let entries_bytes = &FONT_DATA[8..];
    if entries_bytes.len() < num_entries * ENTRY_SIZE {
        return None;
    }

    let target = c as u32;
    let mut low = 0;
    let mut high = num_entries;

    while low < high {
        let mid = low + (high - low) / 2;
        let offset = mid * ENTRY_SIZE;
        let cp = u32::from_le_bytes(entries_bytes[offset..offset + 4].try_into().unwrap());
        if cp == target {
            let glyph_bytes: &'static [u8; 32] = entries_bytes[offset + 4..offset + 36]
                .try_into()
                .unwrap();
            return Some(glyph_bytes);
        } else if cp < target {
            low = mid + 1;
        } else {
            high = mid;
        }
    }
    None
}

/// 查找字符位图，若未命中则返回回退矩形框字模。
pub fn glyph_for_char_or_fallback(c: char) -> &'static [u8; 32] {
    glyph_for_char(c).unwrap_or(&FALLBACK_GLYPH)
}

/// 半角像素块状态：每个终端单元格容纳 2 个垂直像素（上半部与下半部）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelBlock {
    /// 全空（空格）
    Empty,
    /// 上半方块 `▀`
    Upper,
    /// 下半方块 `▄`
    Lower,
    /// 全满方块 `█`
    Full,
}

impl PixelBlock {
    /// 转换为半角字符。
    #[inline]
    pub const fn to_char(self) -> char {
        match self {
            Self::Empty => ' ',
            Self::Upper => '▀',
            Self::Lower => '▄',
            Self::Full => '█',
        }
    }
}

/// 将 16×16 点阵字模转换为 8 行 × 16 列的半角像素网格。
pub fn render_glyph_blocks(glyph: &[u8; 32]) -> [[PixelBlock; 16]; 8] {
    let mut blocks = [[PixelBlock::Empty; 16]; 8];
    for (line, row) in blocks.iter_mut().enumerate() {
        let row_upper = line * 2;
        let row_lower = line * 2 + 1;
        let u_byte0 = glyph[row_upper * 2];
        let u_byte1 = glyph[row_upper * 2 + 1];
        let l_byte0 = glyph[row_lower * 2];
        let l_byte1 = glyph[row_lower * 2 + 1];

        for (col, block) in row.iter_mut().enumerate() {
            let (u_bit, l_bit) = if col < 8 {
                let shift = 7 - col;
                (((u_byte0 >> shift) & 1) == 1, ((l_byte0 >> shift) & 1) == 1)
            } else {
                let shift = 15 - col;
                (((u_byte1 >> shift) & 1) == 1, ((l_byte1 >> shift) & 1) == 1)
            };

            *block = match (u_bit, l_bit) {
                (true, true) => PixelBlock::Full,
                (true, false) => PixelBlock::Upper,
                (false, true) => PixelBlock::Lower,
                (false, false) => PixelBlock::Empty,
            };
        }
    }
    blocks
}

/// 将 16×16 点阵字模转换为 4 行 × 8 列的 Unicode 盲文微点阵网格（每单元格容纳 2×4 像素）。
pub fn render_glyph_braille(glyph: &[u8; 32]) -> [[char; 8]; 4] {
    let mut grid = [['\u{2800}'; 8]; 4];
    const DOTS: [((usize, usize), u32); 8] = [
        ((0, 0), 0x01),
        ((0, 1), 0x02),
        ((0, 2), 0x04),
        ((1, 0), 0x08),
        ((1, 1), 0x10),
        ((1, 2), 0x20),
        ((0, 3), 0x40),
        ((1, 3), 0x80),
    ];

    for (r, row) in grid.iter_mut().enumerate() {
        for (c, cell) in row.iter_mut().enumerate() {
            let mut val = 0u32;
            for &((dx, dy), bit) in &DOTS {
                let px = c * 2 + dx;
                let py = r * 4 + dy;
                let byte_idx = py * 2 + (px / 8);
                let bit_shift = 7 - (px % 8);
                if ((glyph[byte_idx] >> bit_shift) & 1) == 1 {
                    val |= bit;
                }
            }
            *cell = char::from_u32(0x2800 + val).unwrap_or('\u{2800}');
        }
    }
    grid
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builtin_single_chars_all_hit() {
        let sets = [
            include_str!("../data/common-chars-qian.txt"),
            include_str!("../data/common-chars-zhong.txt"),
            include_str!("../data/common-chars-hou.txt"),
            include_str!("../data/kongming-1hit-chars.txt"),
        ];

        for text in sets {
            for c in text.chars().filter(|ch| !ch.is_whitespace()) {
                assert!(
                    glyph_for_char(c).is_some(),
                    "内置字符 '{c}' (U+{:04X}) 应在 font16 字模表中命中",
                    c as u32
                );
            }
        }
    }

    #[test]
    fn test_fallback_glyph_dimensions() {
        let blocks = render_glyph_blocks(&FALLBACK_GLYPH);
        assert_eq!(blocks.len(), 8);
        assert_eq!(blocks[0].len(), 16);
    }

    #[test]
    fn test_braille_glyph_dimensions() {
        let grid = render_glyph_braille(&FALLBACK_GLYPH);
        assert_eq!(grid.len(), 4);
        assert_eq!(grid[0].len(), 8);
        assert!(grid.iter().any(|row| row.iter().any(|&ch| ch != '\u{2800}')));
    }
}
