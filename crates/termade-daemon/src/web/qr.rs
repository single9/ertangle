//! 零外部相依的輕量 QR Code 產生器與終端機 Unicode 半格渲染器。
//! 支援 Version 1 到 6，Byte 模式，Low 容錯率 (ECC L)。

/// GF(256) 常數與對數表 (多項式 0x11D)。
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
        GF_EXP[(GF_LOG[x as usize] as usize + GF_LOG[y as usize] as usize) % 255]
    }
}

/// 計算 Reed-Solomon 糾錯碼 (ECC L)。
fn reed_solomon_compute(data: &[u8], ec_count: usize) -> Vec<u8> {
    // 建立產生多項式 generator polynomial
    let mut gen = vec![1u8];
    for &factor in GF_EXP.iter().take(ec_count) {
        let mut next = vec![0u8; gen.len() + 1];
        for (j, &c) in gen.iter().enumerate() {
            next[j] ^= gf_mul(c, factor);
            next[j + 1] ^= c;
        }
        gen = next;
    }

    let mut rem = vec![0u8; ec_count];
    for &byte in data {
        let factor = byte ^ rem[0];
        rem.copy_within(1..ec_count, 0);
        rem[ec_count - 1] = 0;
        for j in 0..ec_count {
            rem[j] ^= gf_mul(gen[j], factor);
        }
    }
    rem
}

/// QR Code 規格資料 (Version 1 到 6，ECC L)。
struct QrVersion {
    version: usize,
    size: usize,
    #[allow(dead_code)]
    total_codewords: usize,
    data_codewords: usize,
    ec_codewords: usize,
    alignment_pos: &'static [usize],
}

const VERSIONS: [QrVersion; 6] = [
    QrVersion {
        version: 1,
        size: 21,
        total_codewords: 26,
        data_codewords: 19,
        ec_codewords: 7,
        alignment_pos: &[],
    },
    QrVersion {
        version: 2,
        size: 25,
        total_codewords: 44,
        data_codewords: 34,
        ec_codewords: 10,
        alignment_pos: &[6, 18],
    },
    QrVersion {
        version: 3,
        size: 29,
        total_codewords: 70,
        data_codewords: 55,
        ec_codewords: 15,
        alignment_pos: &[6, 22],
    },
    QrVersion {
        version: 4,
        size: 33,
        total_codewords: 100,
        data_codewords: 80,
        ec_codewords: 20,
        alignment_pos: &[6, 26],
    },
    QrVersion {
        version: 5,
        size: 37,
        total_codewords: 134,
        data_codewords: 108,
        ec_codewords: 26,
        alignment_pos: &[6, 30],
    },
    QrVersion {
        version: 6,
        size: 41,
        total_codewords: 172,
        data_codewords: 136,
        ec_codewords: 36,
        alignment_pos: &[6, 34],
    },
];

pub struct QrCode {
    pub size: usize,
    modules: Vec<bool>,
}

impl QrCode {
    pub fn is_dark(&self, row: usize, col: usize) -> bool {
        if row < self.size && col < self.size {
            self.modules[row * self.size + col]
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
        let pad_bytes = [0xEC, 0x11];
        let mut pad_idx = 0;
        while data_bytes.len() < ver.data_codewords {
            data_bytes.push(pad_bytes[pad_idx % 2]);
            pad_idx += 1;
        }

        // 2. 計算糾錯碼
        let ec_bytes = reed_solomon_compute(&data_bytes, ver.ec_codewords);

        // 3. 完整碼字串
        let mut all_bytes = data_bytes;
        all_bytes.extend_from_slice(&ec_bytes);

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
                        m[idx] = Some(is_black);
                        f[idx] = true;
                    }
                }
                // 分隔區 (Separator)
                for r in 0..8 {
                    for c in 0..8 {
                        let rr = top as isize + r as isize - (if top > 0 { 1 } else { 0 });
                        let cc = left as isize + c as isize - (if left > 0 { 1 } else { 0 });
                        if rr >= 0 && rr < sz as isize && cc >= 0 && cc < sz as isize {
                            let idx = (rr as usize) * sz + (cc as usize);
                            if !f[idx] {
                                m[idx] = Some(false);
                                f[idx] = true;
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
                            matrix[idx] = Some(is_b);
                            function[idx] = true;
                        }
                    }
                }
            }
        }

        // 定時圖案 (Timing Patterns: row 6 與 col 6)
        for i in 8..(size - 8) {
            let row_idx = 6 * size + i;
            if !function[row_idx] {
                matrix[row_idx] = Some(i % 2 == 0);
                function[row_idx] = true;
            }
            let col_idx = i * size + 6;
            if !function[col_idx] {
                matrix[col_idx] = Some(i % 2 == 0);
                function[col_idx] = true;
            }
        }

        // 暗模組 (Dark module)
        let dark_idx = (4 * ver.version + 9) * size + 8;
        matrix[dark_idx] = Some(true);
        function[dark_idx] = true;

        // 保留格式資訊區 (Format information area)
        for i in 0..9 {
            if i != 6 {
                function[8 * size + i] = true;
                function[i * size + 8] = true;
            }
        }
        for i in 0..8 {
            function[8 * size + (size - 1 - i)] = true;
            function[(size - 1 - i) * size + 8] = true;
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
                    if !function[idx] {
                        let bit = if bit_idx < all_bits.len() {
                            all_bits[bit_idx]
                        } else {
                            false
                        };
                        bit_idx += 1;
                        // Mask Pattern 0: (r + c) % 2 == 0
                        let mask = (r + c) % 2 == 0;
                        matrix[idx] = Some(bit ^ mask);
                    }
                }
            }
            upward = !upward;
            col -= 2;
        }

        // 6. 填入格式資訊 (ECC L = 01, Mask 0 = 000 -> 01000 + BCH -> 0b111011111000100 ^ 0x5412)
        // 預先計算好的 ECC L, Mask 0 格式字元 15 bits:
        let format_bits: u16 = 0b111011111000100 ^ 0b101010000010010;
        for i in 0..15 {
            let b = (format_bits >> (14 - i)) & 1 == 1;
            // 左上部分
            let (r, c) = if i < 6 {
                (8, i)
            } else if i < 8 {
                (8, i + 1)
            } else if i == 8 {
                (7, 8)
            } else {
                (14 - i, 8)
            };
            matrix[r * size + c] = Some(b);

            // 右上與左下部分
            let (r2, c2) = if i < 8 {
                (size - 1 - i, 8)
            } else {
                (8, size - 15 + i)
            };
            matrix[r2 * size + c2] = Some(b);
        }

        let modules = matrix.into_iter().map(|b| b.unwrap_or(false)).collect();
        Some(QrCode { size, modules })
    }

    /// 渲染為終端機字串 (利用 Unicode 半格字元 `▀`、`▄`、`█` 縮小行數)。
    pub fn render_terminal(&self) -> String {
        let border = 2;
        let full_size = self.size + border * 2;
        let mut out = String::new();

        // 每次處理兩列 (上、下)
        for y in (0..full_size).step_by(2) {
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

                // 黑底終端機 (黑色為模組)：以反白或 Unicode 區塊顯示
                // 上黑下黑: █
                // 上黑下白: ▀
                // 上白下黑: ▄
                // 上白下白: 空白
                match (top_dark, bot_dark) {
                    (true, true) => out.push('█'),
                    (true, false) => out.push('▀'),
                    (false, true) => out.push('▄'),
                    (false, false) => out.push(' '),
                }
            }
            out.push('\n');
        }
        out
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
}
