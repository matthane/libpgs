//! PGS RLE (Run-Length Encoding) bitmap decoder.
//!
//! Decodes ODS object bitmap data from the PGS RLE format into a flat buffer
//! of palette indices (1 byte per pixel, row-major order).

/// Encode a flat palette-index buffer into PGS RLE-compressed bitmap data.
///
/// `pixels` is a flat buffer of palette indices (1 byte per pixel, row-major).
/// `width` and `height` define the image dimensions.
///
/// Returns `None` if `pixels.len() != width * height`.
pub fn encode_rle(pixels: &[u8], width: u16, height: u16) -> Option<Vec<u8>> {
    let w = width as usize;
    let h = height as usize;
    let total = w * h;

    if pixels.len() != total {
        return None;
    }

    if total == 0 {
        return Some(Vec::new());
    }

    let mut buf = Vec::new();

    for row in pixels.chunks(w) {
        let mut col = 0;
        while col < row.len() {
            let color = row[col];
            let mut run = 1usize;
            while col + run < row.len() && row[col + run] == color {
                run += 1;
            }

            // Encode the run, splitting if > 16383
            let mut remaining = run;
            while remaining > 0 {
                let chunk = remaining.min(16383);
                remaining -= chunk;

                if color == 0 {
                    if chunk <= 63 {
                        // 0x00, 0x00..0x3F
                        buf.push(0x00);
                        buf.push(chunk as u8);
                    } else {
                        // 0x00, 0x40|(len>>8), len&0xFF
                        buf.push(0x00);
                        buf.push(0x40 | ((chunk >> 8) as u8));
                        buf.push((chunk & 0xFF) as u8);
                    }
                } else {
                    if chunk == 1 {
                        buf.push(color);
                    } else if chunk == 2 {
                        buf.push(color);
                        buf.push(color);
                    } else if chunk <= 63 {
                        // 0x00, 0x80|length, color
                        buf.push(0x00);
                        buf.push(0x80 | (chunk as u8));
                        buf.push(color);
                    } else {
                        // 0x00, 0xC0|(len>>8), len&0xFF, color
                        buf.push(0x00);
                        buf.push(0xC0 | ((chunk >> 8) as u8));
                        buf.push((chunk & 0xFF) as u8);
                        buf.push(color);
                    }
                }
            }

            col += run;
        }

        // End-of-line marker
        buf.push(0x00);
        buf.push(0x00);
    }

    Some(buf)
}

/// Decode PGS RLE-compressed bitmap data into a flat palette-index buffer.
///
/// `rle_data` is the raw RLE bytes (after the ODS header and width/height fields).
/// `width` and `height` define the expected image dimensions.
///
/// Returns a `Vec<u8>` of length `width * height`, where each byte is a palette
/// entry index (0–255). Pixels are stored in row-major order.
///
/// Returns `None` if the RLE data is malformed or the decoded pixel count
/// does not match `width * height`.
pub fn decode_rle(rle_data: &[u8], width: u16, height: u16) -> Option<Vec<u8>> {
    let w = width as usize;
    let h = height as usize;
    let total = w * h;

    if total == 0 {
        return Some(Vec::new());
    }

    let mut pixels = Vec::with_capacity(total);
    let mut i = 0;
    let len = rle_data.len();

    // Track position within the current row for end-of-line padding.
    let mut col = 0;

    while i < len && pixels.len() < total {
        let byte = rle_data[i];
        i += 1;

        if byte != 0x00 {
            // Non-zero byte: single pixel of color `byte`.
            pixels.push(byte);
            col += 1;
        } else {
            // Zero byte: read the next byte to determine the run type.
            if i >= len {
                break;
            }
            let flag = rle_data[i];
            i += 1;

            if flag == 0x00 {
                // End of line — pad remaining columns with color 0.
                while col < w && pixels.len() < total {
                    pixels.push(0);
                    col += 1;
                }
                col = 0;
            } else {
                let top2 = flag & 0xC0;
                match top2 {
                    0x00 => {
                        // 00 00LLLLLL: L pixels of color 0 (L: 1–63)
                        let run = (flag & 0x3F) as usize;
                        for _ in 0..run {
                            if pixels.len() >= total {
                                break;
                            }
                            pixels.push(0);
                            col += 1;
                        }
                    }
                    0x40 => {
                        // 00 01LLLLLL LLLLLLLL: L pixels of color 0 (L: 64–16383)
                        if i >= len {
                            return None;
                        }
                        let run = (((flag & 0x3F) as usize) << 8) | (rle_data[i] as usize);
                        i += 1;
                        for _ in 0..run {
                            if pixels.len() >= total {
                                break;
                            }
                            pixels.push(0);
                            col += 1;
                        }
                    }
                    0x80 => {
                        // 00 10LLLLLL CCCCCCCC: L pixels of color C (L: 3–63)
                        if i >= len {
                            return None;
                        }
                        let run = (flag & 0x3F) as usize;
                        let color = rle_data[i];
                        i += 1;
                        for _ in 0..run {
                            if pixels.len() >= total {
                                break;
                            }
                            pixels.push(color);
                            col += 1;
                        }
                    }
                    0xC0 => {
                        // 00 11LLLLLL LLLLLLLL CCCCCCCC: L pixels of color C (L: 64–16383)
                        if i + 1 >= len {
                            return None;
                        }
                        let run = (((flag & 0x3F) as usize) << 8) | (rle_data[i] as usize);
                        let color = rle_data[i + 1];
                        i += 2;
                        for _ in 0..run {
                            if pixels.len() >= total {
                                break;
                            }
                            pixels.push(color);
                            col += 1;
                        }
                    }
                    _ => unreachable!(),
                }
            }
        }
    }

    // If the RLE data ended without a final EOL, pad remaining pixels.
    while pixels.len() < total {
        pixels.push(0);
    }

    if pixels.len() == total {
        Some(pixels)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_image() {
        let result = decode_rle(&[], 0, 0);
        assert_eq!(result, Some(vec![]));
    }

    #[test]
    fn single_pixel_nonzero() {
        // Single non-zero byte: one pixel of color 5.
        let data = [0x05];
        let result = decode_rle(&data, 1, 1).unwrap();
        assert_eq!(result, vec![5]);
    }

    #[test]
    fn single_pixel_color_zero_via_run() {
        // Short color-0 run of length 1.
        let data = [0x00, 0x01];
        let result = decode_rle(&data, 1, 1).unwrap();
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn short_color_zero_run() {
        // 00 00000011 = 3 pixels of color 0.
        let data = [0x00, 0x03];
        let result = decode_rle(&data, 3, 1).unwrap();
        assert_eq!(result, vec![0, 0, 0]);
    }

    #[test]
    fn long_color_zero_run() {
        // 00 01000001 00000000 = 256 pixels of color 0.
        let data = [0x00, 0x41, 0x00];
        let result = decode_rle(&data, 256, 1).unwrap();
        assert_eq!(result.len(), 256);
        assert!(result.iter().all(|&p| p == 0));
    }

    #[test]
    fn short_color_c_run() {
        // 00 10000011 FF = 3 pixels of color 255.
        let data = [0x00, 0x83, 0xFF];
        let result = decode_rle(&data, 3, 1).unwrap();
        assert_eq!(result, vec![255, 255, 255]);
    }

    #[test]
    fn long_color_c_run() {
        // 00 11000001 00000000 00000111 = 256 pixels of color 7.
        let data = [0x00, 0xC1, 0x00, 0x07];
        let result = decode_rle(&data, 256, 1).unwrap();
        assert_eq!(result.len(), 256);
        assert!(result.iter().all(|&p| p == 7));
    }

    #[test]
    fn end_of_line_padding() {
        // 4x2 image. Row 1: 2 explicit pixels then EOL (pads 2 zeros).
        // Row 2: 4 explicit pixels.
        let data = [
            0x01, 0x02, // Row 1: pixel 1, pixel 2
            0x00, 0x00, // EOL — pads to width 4
            0x03, 0x04, 0x05, 0x06, // Row 2: 4 pixels
        ];
        let result = decode_rle(&data, 4, 2).unwrap();
        assert_eq!(result, vec![1, 2, 0, 0, 3, 4, 5, 6]);
    }

    #[test]
    fn mixed_runs_multirow() {
        // 3x2 image.
        // Row 1: color 10 (1px), short run of 2 zeros.
        // EOL.
        // Row 2: short color-C run of 3 pixels color 20.
        let data = [
            0x0A,       // pixel color 10
            0x00, 0x02, // 2 pixels of color 0
            0x00, 0x00, // EOL
            0x00, 0x83, 0x14, // 3 pixels of color 20
        ];
        let result = decode_rle(&data, 3, 2).unwrap();
        assert_eq!(result, vec![10, 0, 0, 20, 20, 20]);
    }

    #[test]
    fn truncated_long_zero_run() {
        // Long color-0 run needs 3 bytes total but only 2 provided.
        let data = [0x00, 0x41]; // Missing the low byte.
        assert!(decode_rle(&data, 256, 1).is_none());
    }

    #[test]
    fn truncated_short_color_run() {
        // Short color-C run needs color byte but it's missing.
        let data = [0x00, 0x83]; // Missing color byte.
        assert!(decode_rle(&data, 3, 1).is_none());
    }

    #[test]
    fn truncated_long_color_run() {
        // Long color-C run needs 4 bytes total but only 3 provided.
        let data = [0x00, 0xC1, 0x00]; // Missing color byte.
        assert!(decode_rle(&data, 256, 1).is_none());
    }

    #[test]
    fn rle_data_pads_short_output() {
        // RLE data produces fewer pixels than width*height — remainder padded with 0.
        let data = [0x01]; // 1 pixel of color 1.
        let result = decode_rle(&data, 3, 1).unwrap();
        assert_eq!(result, vec![1, 0, 0]);
    }

    #[test]
    fn max_run_length() {
        // Maximum run length: 16383 pixels of color 42.
        // 00 11111111 11111111 00101010
        let data = [0x00, 0xFF, 0xFF, 0x2A];
        let result = decode_rle(&data, 16383, 1).unwrap();
        assert_eq!(result.len(), 16383);
        assert!(result.iter().all(|&p| p == 42));
    }

    #[test]
    fn color_index_255() {
        // Verify color index 255 is handled (not confused with a flag).
        let data = [0xFF];
        let result = decode_rle(&data, 1, 1).unwrap();
        assert_eq!(result, vec![255]);
    }

    // -- encode_rle tests --

    #[test]
    fn encode_empty_image() {
        assert_eq!(encode_rle(&[], 0, 0), Some(vec![]));
    }

    #[test]
    fn encode_wrong_length_returns_none() {
        assert_eq!(encode_rle(&[0, 0, 0], 2, 1), None);
    }

    #[test]
    fn encode_single_nonzero_pixel() {
        let pixels = vec![42];
        let rle = encode_rle(&pixels, 1, 1).unwrap();
        let decoded = decode_rle(&rle, 1, 1).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_single_zero_pixel() {
        let pixels = vec![0];
        let rle = encode_rle(&pixels, 1, 1).unwrap();
        let decoded = decode_rle(&rle, 1, 1).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_all_zeros() {
        let pixels = vec![0; 100];
        let rle = encode_rle(&pixels, 100, 1).unwrap();
        let decoded = decode_rle(&rle, 100, 1).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_nonzero_run_of_2() {
        // Run of 2 should use two literal bytes (more compact than 3-byte encoding).
        let pixels = vec![5, 5];
        let rle = encode_rle(&pixels, 2, 1).unwrap();
        let decoded = decode_rle(&rle, 2, 1).unwrap();
        assert_eq!(decoded, pixels);
        // Two literal bytes + EOL (0x00, 0x00) = 4 bytes total
        assert_eq!(rle, vec![5, 5, 0x00, 0x00]);
    }

    #[test]
    fn encode_nonzero_run_of_3() {
        let pixels = vec![7, 7, 7];
        let rle = encode_rle(&pixels, 3, 1).unwrap();
        let decoded = decode_rle(&rle, 3, 1).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_long_nonzero_run() {
        let pixels = vec![42; 256];
        let rle = encode_rle(&pixels, 256, 1).unwrap();
        let decoded = decode_rle(&rle, 256, 1).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_mixed_multirow() {
        // 3x2 image: [10, 0, 0] / [20, 20, 20]
        let pixels = vec![10, 0, 0, 20, 20, 20];
        let rle = encode_rle(&pixels, 3, 2).unwrap();
        let decoded = decode_rle(&rle, 3, 2).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_run_exceeding_16383() {
        // A run longer than 16383 must be split.
        let pixels = vec![0; 20000];
        let rle = encode_rle(&pixels, 20000, 1).unwrap();
        let decoded = decode_rle(&rle, 20000, 1).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_nonzero_run_exceeding_16383() {
        let pixels = vec![99; 20000];
        let rle = encode_rle(&pixels, 20000, 1).unwrap();
        let decoded = decode_rle(&rle, 20000, 1).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_alternating_colors() {
        let pixels: Vec<u8> = (0..10).map(|i| if i % 2 == 0 { 1 } else { 2 }).collect();
        let rle = encode_rle(&pixels, 10, 1).unwrap();
        let decoded = decode_rle(&rle, 10, 1).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_long_zero_run_64() {
        // Exactly 64 zeros — should use the long (3-byte) encoding.
        let pixels = vec![0; 64];
        let rle = encode_rle(&pixels, 64, 1).unwrap();
        let decoded = decode_rle(&rle, 64, 1).unwrap();
        assert_eq!(decoded, pixels);
    }

    #[test]
    fn encode_max_run_16383() {
        let pixels = vec![42; 16383];
        let rle = encode_rle(&pixels, 16383, 1).unwrap();
        let decoded = decode_rle(&rle, 16383, 1).unwrap();
        assert_eq!(decoded, pixels);
    }
}
