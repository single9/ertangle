//! 零外部相依的輕量 QR Code 產生器與終端機 Unicode 半格渲染器。
//! 支援 Version 1 到 9，Byte 模式，Low 容錯率 (ECC L)。

/// GF(256) 常數與對數表 (多項式 0x11D)。
#[expect(
    clippy::indexing_slicing,
    reason = "compile-time constant table initialization with bounded loops"
)]
const GF_EXP: [u8; 512] = {
    let mut exp = [0u8; 512];
    let mut x = 1u16;
    let mut i = 0;
    while i < 255 {
        exp[i] = x as u8;
        exp[i + 255] = x as u8;
        x <<= 1;
        if x & 0x100 != 0 {
            x ^= 0x11D;
        }
        i += 1;
    }
    exp
};

#[expect(
    clippy::indexing_slicing,
    reason = "compile-time constant table initialization with bounded loops"
)]
const GF_LOG: [u8; 256] = {
    let mut log = [0u8; 256];
    let mut i = 0;
    while i < 255 {
        log[GF_EXP[i] as usize] = i as u8;
        i += 1;
    }
    log
};

fn gf_mul(x: u8, y: u8) -> u8 {
    if x == 0 || y == 0 {
        0
    } else {
        let log_x = GF_LOG.get(x as usize).copied().unwrap_or(0) as usize;
        let log_y = GF_LOG.get(y as usize).copied().unwrap_or(0) as usize;
        GF_EXP.get((log_x + log_y) % 255).copied().unwrap_or(0)
    }
}

/// 計算 Reed-Solomon 糾錯碼 (ECC L)。
fn reed_solomon_compute(data: &[u8], ec_count: usize) -> Vec<u8> {
    // 建立產生多項式 generator polynomial
    let mut gen = vec![1u8];
    for &factor in GF_EXP.iter().take(ec_count) {
        let mut next = vec![0u8; gen.len() + 1];
        for (j, &c) in gen.iter().enumerate() {
            if let Some(n) = next.get_mut(j) {
                *n ^= gf_mul(c, factor);
            }
            if let Some(n) = next.get_mut(j + 1) {
                *n ^= c;
            }
        }
        gen = next;
    }
    // 轉為由最高次項開始排列 (首項恆為 1，略過)
    gen.reverse();

    let mut rem = vec![0u8; ec_count];
    for &byte in data {
        let first_rem = rem.first().copied().unwrap_or(0);
        let factor = byte ^ first_rem;
        if ec_count > 1 {
            rem.copy_within(1..ec_count, 0);
        }
        if let Some(last) = rem.get_mut(ec_count - 1) {
            *last = 0;
        }
        for (j, rem_j) in rem.iter_mut().enumerate().take(ec_count) {
            let gen_val = gen.get(j + 1).copied().unwrap_or(0);
            *rem_j ^= gf_mul(gen_val, factor);
        }
    }
    rem
}

/// QR Code 規格資料 (Version 1 到 6，ECC L)。
struct QrVersion {
    version: usize,
    size: usize,
    total_codewords: usize,
    data_codewords: usize,
    ec_codewords: usize,
    blocks: usize,
    alignment_pos: &'static [usize],
}

const VERSIONS: [QrVersion; 9] = [
    QrVersion {
        version: 1,
        size: 21,
        total_codewords: 26,
        data_codewords: 19,
        ec_codewords: 7,
        blocks: 1,
        alignment_pos: &[],
    },
    QrVersion {
        version: 2,
        size: 25,
        total_codewords: 44,
        data_codewords: 34,
        ec_codewords: 10,
        blocks: 1,
        alignment_pos: &[6, 18],
    },
    QrVersion {
        version: 3,
        size: 29,
        total_codewords: 70,
        data_codewords: 55,
        ec_codewords: 15,
        blocks: 1,
        alignment_pos: &[6, 22],
    },
    QrVersion {
        version: 4,
        size: 33,
        total_codewords: 100,
        data_codewords: 80,
        ec_codewords: 20,
        blocks: 1,
        alignment_pos: &[6, 26],
    },
    QrVersion {
        version: 5,
        size: 37,
        total_codewords: 134,
        data_codewords: 108,
        ec_codewords: 26,
        blocks: 1,
        alignment_pos: &[6, 30],
    },
    QrVersion {
        version: 6,
        size: 41,
        total_codewords: 172,
        data_codewords: 136,
        ec_codewords: 36,
        blocks: 2,
        alignment_pos: &[6, 34],
    },
    QrVersion {
        version: 7,
        size: 45,
        total_codewords: 196,
        data_codewords: 156,
        ec_codewords: 40,
        blocks: 2,
        alignment_pos: &[6, 22, 38],
    },
    QrVersion {
        version: 8,
        size: 49,
        total_codewords: 242,
        data_codewords: 194,
        ec_codewords: 48,
        blocks: 2,
        alignment_pos: &[6, 24, 42],
    },
    QrVersion {
        version: 9,
        size: 53,
        total_codewords: 292,
        data_codewords: 232,
        ec_codewords: 60,
        blocks: 2,
        alignment_pos: &[6, 26, 46],
    },
];

pub struct QrCode {
    pub size: usize,
    modules: Vec<bool>,
}

impl QrCode {
    pub fn is_dark(&self, row: usize, col: usize) -> bool {
        if row < self.size && col < self.size {
            self.modules
                .get(row * self.size + col)
                .copied()
                .unwrap_or(false)
        } else {
            false
        }
    }

    /// 將文字編碼為 QR Code 模組矩陣。
    pub fn encode(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        let ver = VERSIONS
            .iter()
            .find(|v| bytes.len() + 3 <= v.data_codewords)?;

        // 1. 資料位元流 (Byte 模式：0100 + 長度 8 bits + 資料 + 終止碼 0000 + 填補)
        let mut bits = Vec::with_capacity(ver.data_codewords * 8);
        // Byte 模式指示碼 0100
        bits.extend_from_slice(&[false, true, false, false]);
        // 長度 8 bits
        let len = bytes.len() as u8;
        for i in (0..8).rev() {
            bits.push((len >> i) & 1 == 1);
        }
        // 資料位元組
        for &b in bytes {
            for i in (0..8).rev() {
                bits.push((b >> i) & 1 == 1);
            }
        }
        // 終止碼 (最多 4 個 0)
        let max_bits = ver.data_codewords * 8;
        for _ in 0..4 {
            if bits.len() < max_bits {
                bits.push(false);
            }
        }
        // 補齊到 8 的倍數
        while bits.len() % 8 != 0 && bits.len() < max_bits {
            bits.push(false);
        }
        // 轉為位元組
        let mut data_bytes = Vec::with_capacity(ver.data_codewords);
        for chunk in bits.chunks(8) {
            let mut val = 0u8;
            for &b in chunk {
                val = (val << 1) | (b as u8);
            }
            data_bytes.push(val);
        }
        // 交替填補碼 0xEC 與 0x11
        let mut pad_idx = 0;
        while data_bytes.len() < ver.data_codewords {
            data_bytes.push(if pad_idx % 2 == 0 { 0xEC } else { 0x11 });
            pad_idx += 1;
        }

        // 2. 計算糾錯碼與多區塊交錯 (Interleaving)
        let mut all_bytes = Vec::with_capacity(ver.total_codewords);
        if ver.blocks <= 1 {
            let ec_bytes = reed_solomon_compute(&data_bytes, ver.ec_codewords);
            all_bytes.extend(data_bytes);
            all_bytes.extend(ec_bytes);
        } else {
            let num_blocks = ver.blocks;
            let data_per_block = ver.data_codewords / num_blocks;
            let ec_per_block = ver.ec_codewords / num_blocks;

            let mut block_data = Vec::with_capacity(num_blocks);
            let mut block_ec = Vec::with_capacity(num_blocks);
            for b in 0..num_blocks {
                let start = b * data_per_block;
                let end = start + data_per_block;
                let slice = data_bytes.get(start..end).unwrap_or_default();
                let ec = reed_solomon_compute(slice, ec_per_block);
                block_data.push(slice);
                block_ec.push(ec);
            }

            // 交錯排列資料位元組
            for i in 0..data_per_block {
                for b in 0..num_blocks {
                    let val = block_data
                        .get(b)
                        .and_then(|blk| blk.get(i))
                        .copied()
                        .unwrap_or(0);
                    all_bytes.push(val);
                }
            }
            // 交錯排列糾錯位元組
            for i in 0..ec_per_block {
                for b in 0..num_blocks {
                    let val = block_ec
                        .get(b)
                        .and_then(|blk| blk.get(i))
                        .copied()
                        .unwrap_or(0);
                    all_bytes.push(val);
                }
            }
        }

        let mut all_bits = Vec::with_capacity(all_bytes.len() * 8);
        for b in all_bytes {
            for i in (0..8).rev() {
                all_bits.push((b >> i) & 1 == 1);
            }
        }

        // 4. 建立矩陣與遮罩
        let size = ver.size;
        let mut matrix = vec![None; size * size];
        let mut function = vec![false; size * size];

        // 尋找圖案 (Finder Patterns: 3 個角落的 7x7)
        let set_finder =
            |m: &mut [Option<bool>], f: &mut [bool], top: usize, left: usize, sz: usize| {
                for r in 0..7 {
                    for c in 0..7 {
                        let is_black = r == 0
                            || r == 6
                            || c == 0
                            || c == 6
                            || ((2..=4).contains(&r) && (2..=4).contains(&c));
                        let idx = (top + r) * sz + (left + c);
                        if let Some(slot) = m.get_mut(idx) {
                            *slot = Some(is_black);
                        }
                        if let Some(slot) = f.get_mut(idx) {
                            *slot = true;
                        }
                    }
                }
                // 分隔區 (Separator)
                for r in 0..8 {
                    for c in 0..8 {
                        let rr = top as isize + r as isize - (if top > 0 { 1 } else { 0 });
                        let cc = left as isize + c as isize - (if left > 0 { 1 } else { 0 });
                        if rr >= 0 && rr < sz as isize && cc >= 0 && cc < sz as isize {
                            let idx = (rr as usize) * sz + (cc as usize);
                            if !f.get(idx).copied().unwrap_or(true) {
                                if let Some(slot) = m.get_mut(idx) {
                                    *slot = Some(false);
                                }
                                if let Some(slot) = f.get_mut(idx) {
                                    *slot = true;
                                }
                            }
                        }
                    }
                }
            };

        set_finder(&mut matrix, &mut function, 0, 0, size);
        set_finder(&mut matrix, &mut function, 0, size - 7, size);
        set_finder(&mut matrix, &mut function, size - 7, 0, size);

        // 對齊圖案 (Alignment Patterns for v2+)
        if ver.version >= 2 && ver.alignment_pos.len() >= 2 {
            let pos = ver.alignment_pos;
            for &r in pos {
                for &c in pos {
                    // 若與尋找圖案重疊則略過
                    if (r < 9 && c < 9) || (r < 9 && c >= size - 9) || (r >= size - 9 && c < 9) {
                        continue;
                    }
                    for dr in 0..5 {
                        for dc in 0..5 {
                            let is_b =
                                dr == 0 || dr == 4 || dc == 0 || dc == 4 || (dr == 2 && dc == 2);
                            let idx = (r - 2 + dr) * size + (c - 2 + dc);
                            if let Some(slot) = matrix.get_mut(idx) {
                                *slot = Some(is_b);
                            }
                            if let Some(slot) = function.get_mut(idx) {
                                *slot = true;
                            }
                        }
                    }
                }
            }
        }

        // 定時圖案 (Timing Patterns: row 6 與 col 6)
        for i in 8..(size - 8) {
            let row_idx = 6 * size + i;
            if !function.get(row_idx).copied().unwrap_or(true) {
                if let Some(slot) = matrix.get_mut(row_idx) {
                    *slot = Some(i % 2 == 0);
                }
                if let Some(slot) = function.get_mut(row_idx) {
                    *slot = true;
                }
            }
            let col_idx = i * size + 6;
            if !function.get(col_idx).copied().unwrap_or(true) {
                if let Some(slot) = matrix.get_mut(col_idx) {
                    *slot = Some(i % 2 == 0);
                }
                if let Some(slot) = function.get_mut(col_idx) {
                    *slot = true;
                }
            }
        }

        // 暗模組 (Dark module)
        let dark_idx = (4 * ver.version + 9) * size + 8;
        if let Some(slot) = matrix.get_mut(dark_idx) {
            *slot = Some(true);
        }
        if let Some(slot) = function.get_mut(dark_idx) {
            *slot = true;
        }

        // 保留格式資訊區 (Format information area)
        for i in 0..9 {
            if i != 6 {
                if let Some(slot) = function.get_mut(8 * size + i) {
                    *slot = true;
                }
                if let Some(slot) = function.get_mut(i * size + 8) {
                    *slot = true;
                }
            }
        }
        for i in 0..8 {
            if let Some(slot) = function.get_mut(8 * size + (size - 1 - i)) {
                *slot = true;
            }
            if let Some(slot) = function.get_mut((size - 1 - i) * size + 8) {
                *slot = true;
            }
        }

        // Version 7+ 需要版本資訊區 (右上角 3x6 與左下角 6x3)，先保留避免資料寫入。
        if ver.version >= 7 {
            for i in 0..18 {
                let a = size - 11 + i % 3;
                let b = i / 3;
                if let Some(slot) = function.get_mut(b * size + a) {
                    *slot = true;
                }
                if let Some(slot) = function.get_mut(a * size + b) {
                    *slot = true;
                }
            }
        }

        // 5. 填入資料位元 (交錯直行向上向下填入，並套用遮罩 Pattern 0: (row + col) % 2 == 0)
        let mut bit_idx = 0;
        let mut col = size as isize - 1;
        let mut upward = true;

        while col > 0 {
            if col == 6 {
                col -= 1;
            }
            let rows: Vec<usize> = if upward {
                (0..size).rev().collect()
            } else {
                (0..size).collect()
            };

            for r in rows {
                for c in [col as usize, (col - 1) as usize] {
                    let idx = r * size + c;
                    if !function.get(idx).copied().unwrap_or(true) {
                        let bit = all_bits.get(bit_idx).copied().unwrap_or(false);
                        bit_idx += 1;
                        // Mask Pattern 0: (r + c) % 2 == 0
                        let mask = (r + c) % 2 == 0;
                        if let Some(slot) = matrix.get_mut(idx) {
                            *slot = Some(bit ^ mask);
                        }
                    }
                }
            }
            upward = !upward;
            col -= 2;
        }

        // 6. 填入格式資訊 (ECC L = 01, Mask 0 = 000 -> 01000 + BCH -> 0b010001111010110 ^ 0x5412 = 0b111011111000100)
        // 預先計算好的 ECC L, Mask 0 遮罩後格式字元 15 bits：
        let format_bits: u16 = 0b111011111000100;
        for i in 0..15 {
            let b = (format_bits >> i) & 1 == 1;
            // 左上部分 (row, col)
            let (r, c) = if i < 6 {
                (i, 8)
            } else if i < 8 {
                (i + 1, 8)
            } else if i == 8 {
                (8, 7)
            } else {
                (8, 14 - i)
            };
            if let Some(slot) = matrix.get_mut(r * size + c) {
                *slot = Some(b);
            }

            // 右上與左下部分
            let (r2, c2) = if i < 8 {
                (8, size - 1 - i)
            } else {
                (size - 15 + i, 8)
            };
            if let Some(slot) = matrix.get_mut(r2 * size + c2) {
                *slot = Some(b);
            }
        }

        // 7. 版本資訊 (Version 7+)：6 bits 版本號 + 12 bits BCH (產生多項式 0x1F25)。
        if ver.version >= 7 {
            let mut rem = ver.version;
            for _ in 0..12 {
                rem = (rem << 1) ^ (if (rem >> 11) & 1 == 1 { 0x1F25 } else { 0 });
            }
            let version_bits: u32 = ((ver.version << 12) | rem) as u32;
            for i in 0..18 {
                let b = (version_bits >> i) & 1 == 1;
                let a = size - 11 + i % 3;
                let row = i / 3;
                if let Some(slot) = matrix.get_mut(row * size + a) {
                    *slot = Some(b);
                }
                if let Some(slot) = matrix.get_mut(a * size + row) {
                    *slot = Some(b);
                }
            }
        }

        let modules = matrix.into_iter().map(|b| b.unwrap_or(false)).collect();
        Some(QrCode { size, modules })
    }

    /// 渲染為終端機字串 (利用 Unicode 半格字元 `▀`、`▄`、`█` 縮小行數，以 ANSI 白底黑字輸出標準正向 QR Code)。
    pub fn render_terminal(&self) -> String {
        let border = 2;
        let full_size = self.size + border * 2;
        let mut out = String::new();

        // 每次處理兩列 (上、下)
        for y in (0..full_size).step_by(2) {
            out.push_str("\x1b[47;30m");
            for x in 0..full_size {
                let top_dark = if y < border
                    || y >= border + self.size
                    || x < border
                    || x >= border + self.size
                {
                    false
                } else {
                    self.is_dark(y - border, x - border)
                };

                let bot_y = y + 1;
                let bot_dark = if bot_y < border
                    || bot_y >= border + self.size
                    || x < border
                    || x >= border + self.size
                {
                    false
                } else {
                    self.is_dark(bot_y - border, x - border)
                };

                // 在白底黑字下：
                // top_dark (黑) + bot_dark (黑) => █ (全黑前景)
                // top_dark (黑) + bot_dark (白) => ▀ (上半黑前景，下半白背景)
                // top_dark (白) + bot_dark (黑) => ▄ (上半白背景，下半黑前景)
                // top_dark (白) + bot_dark (白) => ' ' (全白背景，包含靜區)
                match (top_dark, bot_dark) {
                    (true, true) => out.push('█'),
                    (true, false) => out.push('▀'),
                    (false, true) => out.push('▄'),
                    (false, false) => out.push(' '),
                }
            }
            out.push_str("\x1b[0m\n");
        }
        out
    }

    /// 渲染為純字元二維陣列 (供 TUI 等自訂前景與背景色之渲染引擎使用)。
    pub fn render_lines(&self) -> Vec<String> {
        let border = 2;
        let full_size = self.size + border * 2;
        let mut lines = Vec::new();

        for y in (0..full_size).step_by(2) {
            let mut line = String::new();
            for x in 0..full_size {
                let top_dark = if y < border
                    || y >= border + self.size
                    || x < border
                    || x >= border + self.size
                {
                    false
                } else {
                    self.is_dark(y - border, x - border)
                };

                let bot_y = y + 1;
                let bot_dark = if bot_y < border
                    || bot_y >= border + self.size
                    || x < border
                    || x >= border + self.size
                {
                    false
                } else {
                    self.is_dark(bot_y - border, x - border)
                };

                match (top_dark, bot_dark) {
                    (true, true) => line.push('█'),
                    (true, false) => line.push('▀'),
                    (false, true) => line.push('▄'),
                    (false, false) => line.push(' '),
                }
            }
            lines.push(line);
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_qr_encode_and_render() {
        let text = "http://127.0.0.1:7421/#token=secret_test_token";
        let qr = QrCode::encode(text).expect("編碼應成功");
        assert!(qr.size >= 21);
        let rendered = qr.render_terminal();
        assert!(!rendered.is_empty());
        assert!(rendered.contains('█') || rendered.contains('▀') || rendered.contains('▄'));
    }

    #[test]
    fn test_project_scoped_web_url_fits_qr() {
        // 含完整專案路徑的 :web qr 網址必須仍能編碼 (T40 曾讓網址超過 v6 容量)。
        let token = "696f5524c18f1c5108d815accb60db754fae5f1ec6d43639584f5b4dea9e255e";
        let url = format!(
            "http://100.64.100.100:7421/?token={token}&project=%2Fworkspace%2Fprojects%2Fexample-organization%2Frepo"
        );
        let qr = QrCode::encode(&url).expect("長 URL 應可編碼");
        assert!(qr.size >= 45);
        assert!(!qr.render_lines().is_empty());
    }

    #[test]
    fn test_version_info_bits() {
        // QR 規格附錄：version 7 與 8 的 18-bit 版本資訊 (BCH 0x1F25)。
        for (version, expected) in [(7usize, 0b000111110010010100u32), (8, 0b001000010110111100)] {
            let mut rem = version;
            for _ in 0..12 {
                rem = (rem << 1) ^ (if (rem >> 11) & 1 == 1 { 0x1F25 } else { 0 });
            }
            let bits: u32 = ((version << 12) | rem) as u32;
            assert_eq!(bits, expected, "version {version}");
        }
    }

    #[test]
    fn test_reed_solomon_known_vector() {
        let data = [
            32, 91, 11, 120, 209, 114, 220, 77, 67, 64, 236, 17, 236, 17, 236, 17,
        ];
        let ec = reed_solomon_compute(&data, 10);
        assert_eq!(ec, [196, 35, 39, 119, 235, 215, 231, 226, 93, 23]);
    }
}
