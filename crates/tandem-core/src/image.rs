//! Clipboard pictures: Windows keeps a copied picture as a DIB (the classic bitmap
//! format Paint, Word, Discord and most apps read and write) and only some apps also
//! offer "PNG". We send PNG over the network, so this converts both ways.
//!
//! Pure Rust and platform independent, so it is tested on every machine.

use std::io::Cursor;

/// Never decode something that would need more than this many pixels.
const MAX_PIXELS: u64 = 64 * 1024 * 1024;

fn u16le(b: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?))
}
fn u32le(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}
fn i32le(b: &[u8], at: usize) -> Option<i32> {
    Some(i32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

/// `CF_DIB` / `CF_DIBV5` bytes (a BITMAPINFOHEADER and the pixels, no file header) to PNG.
/// Handles 24 and 32 bits per pixel, which is what screenshots and "copy image" give.
pub fn dib_to_png(dib: &[u8]) -> Option<Vec<u8>> {
    let header = u32le(dib, 0)? as usize;
    if header < 40 || header > 124 || dib.len() < header {
        return None;
    }
    let width = i32le(dib, 4)?;
    let height = i32le(dib, 8)?;
    let bpp = u16le(dib, 14)? as usize;
    let compression = u32le(dib, 16)?;
    let colors_used = u32le(dib, 32)? as usize;
    if width <= 0 || height == 0 || (bpp != 24 && bpp != 32) {
        return None;
    }
    // 0 = BI_RGB, 3 = BI_BITFIELDS (only the usual BGRA layout is understood).
    if compression != 0 && compression != 3 {
        return None;
    }
    let (w, h) = (width as usize, height.unsigned_abs() as usize);
    if (w as u64) * (h as u64) > MAX_PIXELS {
        return None;
    }
    let top_down = height < 0;

    // Pixels start after the header, the colour masks (BI_BITFIELDS with a 40-byte
    // header) and the palette.
    let mut off = header;
    if compression == 3 && header == 40 {
        off += 12;
    }
    off += colors_used * 4;

    let stride = (w * bpp / 8 + 3) & !3;
    if dib.len() < off.checked_add(stride.checked_mul(h)?)? {
        return None;
    }
    let bytes = bpp / 8;
    let mut rgba = Vec::with_capacity(w * h * 4);
    let mut any_alpha = false;
    for row in 0..h {
        let src_row = if top_down { row } else { h - 1 - row };
        let line = &dib[off + src_row * stride..off + src_row * stride + w * bytes];
        for px in line.chunks_exact(bytes) {
            let a = if bytes == 4 { px[3] } else { 255 };
            any_alpha |= a != 0;
            rgba.extend_from_slice(&[px[2], px[1], px[0], a]);
        }
    }
    // Many apps leave the fourth byte at zero: that means "no transparency", not
    // "invisible".
    if !any_alpha {
        for px in rgba.chunks_exact_mut(4) {
            px[3] = 255;
        }
    }
    encode_png(&rgba, w as u32, h as u32)
}

fn encode_png(rgba: &[u8], w: u32, h: u32) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(Cursor::new(&mut out), w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        enc.set_compression(png::Compression::Fast);
        let mut writer = enc.write_header().ok()?;
        writer.write_image_data(rgba).ok()?;
        writer.finish().ok()?;
    }
    Some(out)
}

/// PNG to `CF_DIB` bytes: a 32-bit bottom-up bitmap, transparency flattened onto white
/// (the PNG itself is still put on the clipboard too, so apps that read it keep alpha).
pub fn png_to_dib(png_bytes: &[u8]) -> Option<Vec<u8>> {
    let mut dec = png::Decoder::new(Cursor::new(png_bytes));
    dec.set_transformations(png::Transformations::EXPAND | png::Transformations::STRIP_16);
    let mut reader = dec.read_info().ok()?;
    let (w, h) = {
        let i = reader.info();
        (i.width as usize, i.height as usize)
    };
    if w == 0 || h == 0 || (w as u64) * (h as u64) > MAX_PIXELS {
        return None;
    }
    let mut buf = vec![0u8; reader.output_buffer_size()];
    let fi = reader.next_frame(&mut buf).ok()?;
    let ch = match fi.color_type {
        png::ColorType::Rgba => 4,
        png::ColorType::Rgb => 3,
        png::ColorType::GrayscaleAlpha => 2,
        png::ColorType::Grayscale => 1,
        png::ColorType::Indexed => return None,
    };
    let mut dib = Vec::with_capacity(40 + w * h * 4);
    dib.extend_from_slice(&40u32.to_le_bytes());
    dib.extend_from_slice(&(w as i32).to_le_bytes());
    dib.extend_from_slice(&(h as i32).to_le_bytes()); // positive: bottom-up
    dib.extend_from_slice(&1u16.to_le_bytes());
    dib.extend_from_slice(&32u16.to_le_bytes());
    dib.extend_from_slice(&0u32.to_le_bytes()); // BI_RGB
    dib.extend_from_slice(&((w * h * 4) as u32).to_le_bytes());
    dib.extend_from_slice(&[0u8; 16]); // resolution, colours used / important
    let stride = fi.line_size;
    for row in (0..h).rev() {
        let line = &buf[row * stride..row * stride + w * ch];
        for px in line.chunks_exact(ch) {
            let (r, g, b, a) = match ch {
                4 => (px[0], px[1], px[2], px[3]),
                3 => (px[0], px[1], px[2], 255),
                2 => (px[0], px[0], px[0], px[1]),
                _ => (px[0], px[0], px[0], 255),
            };
            let flat = |c: u8| -> u8 {
                let (c, a) = (c as u32, a as u32);
                ((c * a + 255 * (255 - a) + 127) / 255) as u8
            };
            dib.extend_from_slice(&[flat(b), flat(g), flat(r), 255]);
        }
    }
    Some(dib)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_png(w: u32, h: u32) -> Vec<u8> {
        let mut rgba = Vec::new();
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[(x * 40) as u8, (y * 60) as u8, 200, 255]);
            }
        }
        encode_png(&rgba, w, h).unwrap()
    }

    fn pixels(png_bytes: &[u8]) -> (u32, u32, Vec<u8>) {
        let mut r = png::Decoder::new(Cursor::new(png_bytes)).read_info().unwrap();
        let mut buf = vec![0u8; r.output_buffer_size()];
        let i = r.next_frame(&mut buf).unwrap();
        (i.width, i.height, buf[..i.buffer_size()].to_vec())
    }

    #[test]
    fn a_picture_survives_png_to_dib_and_back() {
        let png = sample_png(5, 3);
        let dib = png_to_dib(&png).expect("dib");
        let back = dib_to_png(&dib).expect("png");
        assert_eq!(pixels(&png), pixels(&back));
    }

    #[test]
    fn dib_rows_are_bottom_up_and_bgr() {
        // 2x2, top row red then green, bottom row blue then white.
        let rgba = [
            255, 0, 0, 255, 0, 255, 0, 255, //
            0, 0, 255, 255, 255, 255, 255, 255,
        ];
        let png = encode_png(&rgba, 2, 2).unwrap();
        let dib = png_to_dib(&png).unwrap();
        assert_eq!(&dib[40..44], &[255, 0, 0, 255]); // bottom-left: blue as BGRA
        assert_eq!(&dib[48..52], &[0, 0, 255, 255]); // top-left: red as BGRA
    }

    #[test]
    fn a_24_bit_dib_with_row_padding_reads_correctly() {
        // 3 pixels wide => 9 bytes per row, padded to 12.
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&3i32.to_le_bytes());
        dib.extend_from_slice(&(-1i32).to_le_bytes()); // top-down, one row
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&24u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]);
        dib.extend_from_slice(&[0, 0, 255, 0, 255, 0, 255, 0, 0, 0, 0, 0]); // R, G, B + pad
        let png = dib_to_png(&dib).unwrap();
        let (w, h, px) = pixels(&png);
        assert_eq!((w, h), (3, 1));
        assert_eq!(&px[..12], &[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255]);
    }

    #[test]
    fn a_32_bit_dib_with_empty_alpha_is_opaque() {
        let mut dib = Vec::new();
        dib.extend_from_slice(&40u32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1i32.to_le_bytes());
        dib.extend_from_slice(&1u16.to_le_bytes());
        dib.extend_from_slice(&32u16.to_le_bytes());
        dib.extend_from_slice(&[0u8; 24]);
        dib.extend_from_slice(&[10, 20, 30, 0]);
        let (_, _, px) = pixels(&dib_to_png(&dib).unwrap());
        assert_eq!(px, vec![30, 20, 10, 255]);
    }

    #[test]
    fn nonsense_is_refused_not_crashed_on() {
        assert!(dib_to_png(&[]).is_none());
        assert!(dib_to_png(&[1, 2, 3, 4, 5]).is_none());
        assert!(png_to_dib(b"not a png").is_none());
        // Claims a huge picture but carries no pixels.
        let mut dib = vec![0u8; 40];
        dib[0] = 40;
        dib[4..8].copy_from_slice(&60000i32.to_le_bytes());
        dib[8..12].copy_from_slice(&60000i32.to_le_bytes());
        dib[14] = 32;
        assert!(dib_to_png(&dib).is_none());
    }
}
