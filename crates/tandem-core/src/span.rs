//! Dragging a window across the edge, like a second monitor: the window hangs over the
//! edge while you drag, and the other computer shows the part that is past it. Let go
//! more than halfway over and the program moves there (see "move apps").
//!
//! The window only runs on one computer, so the other one shows a live picture of it:
//! JPEG frames a few times a second, plus where it is on every mouse move. This file is
//! the platform-independent part: the geometry, the pictures and the pointer drawn on
//! them. It is pure, so it is tested on every machine.

use crate::platform::{Screen, SpanView};
use crate::proto::Edge;

/// Largest picture sent (it is shown at full size on the other side, just softer).
pub const FRAME_MAX_W: u32 = 1280;
pub const FRAME_MAX_H: u32 = 900;
pub const FRAME_QUALITY: u8 = 62;
/// How often a picture is taken while the window hangs across.
pub const FRAME_EVERY_MS: u64 = 90;

/// A window hanging across `edge` of `scr`: its size and where the pointer holds it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Geo {
    pub edge: Edge,
    pub scr: Screen,
    pub w: i32,
    pub h: i32,
    /// The pointer, inside the window.
    pub gx: i32,
    pub gy: i32,
}

impl Geo {
    fn horizontal(&self) -> bool {
        matches!(self.edge, Edge::Left | Edge::Right)
    }

    /// The window's size across the edge.
    fn depth(&self) -> i32 {
        if self.horizontal() {
            self.w
        } else {
            self.h
        }
    }

    /// Keeps the (virtual) pointer on this screen, except across the edge, where the
    /// window may go until it is entirely on the other side.
    pub fn clamp(&self, vx: i32, vy: i32) -> (i32, i32) {
        let s = self.scr;
        let (right, bottom) = (s.x + s.width, s.y + s.height);
        let cx = |lo: i32, hi: i32, v: i32| v.max(lo).min(hi.max(lo));
        match self.edge {
            Edge::Right => (cx(s.x, right + self.gx, vx), cx(s.y, bottom - 1, vy)),
            Edge::Left => (
                cx(s.x + self.gx - self.w, right - 1, vx),
                cx(s.y, bottom - 1, vy),
            ),
            Edge::Bottom => (cx(s.x, right - 1, vx), cx(s.y, bottom + self.gy, vy)),
            Edge::Top => (
                cx(s.x, right - 1, vx),
                cx(s.y + self.gy - self.h, bottom - 1, vy),
            ),
            _ => (cx(s.x, right - 1, vx), cx(s.y, bottom - 1, vy)),
        }
    }

    /// How many pixels of the window are past the edge (negative: still that far inside).
    pub fn crossed(&self, vx: i32, vy: i32) -> i32 {
        let s = self.scr;
        let (left, top) = (vx - self.gx, vy - self.gy);
        match self.edge {
            Edge::Right => left + self.w - (s.x + s.width),
            Edge::Left => s.x - left,
            Edge::Bottom => top + self.h - (s.y + s.height),
            Edge::Top => s.y - top,
            _ => 0,
        }
    }

    /// Where the window is along the edge, as a fraction of the screen.
    pub fn along(&self, vx: i32, vy: i32) -> f32 {
        let s = self.scr;
        if self.horizontal() {
            (vy - self.gy - s.y) as f32 / s.height.max(1) as f32
        } else {
            (vx - self.gx - s.x) as f32 / s.width.max(1) as f32
        }
    }

    /// Let go here: does the program move over (more than half of it is past the edge)?
    pub fn moved(&self, vx: i32, vy: i32) -> bool {
        self.crossed(vx, vy) * 2 > self.depth()
    }

    /// The pointer moved back just enough for the whole window to be on this screen.
    pub fn back_inside(&self, vx: i32, vy: i32) -> (i32, i32) {
        let c = self.crossed(vx, vy).max(0);
        match self.edge {
            Edge::Right => (vx - c, vy),
            Edge::Left => (vx + c, vy),
            Edge::Bottom => (vx, vy - c),
            Edge::Top => (vx, vy + c),
            _ => (vx, vy),
        }
    }

    /// The edge as a 0..1 position for `Crossed` (where the pointer goes over).
    pub fn frac(&self, vx: i32, vy: i32) -> f32 {
        let s = self.scr;
        let f = if self.horizontal() {
            (vy - s.y) as f32 / (s.height - 1).max(1) as f32
        } else {
            (vx - s.x) as f32 / (s.width - 1).max(1) as f32
        };
        f.clamp(0.0, 1.0)
    }
}

/// Where the window's top-left goes on the receiving screen.
pub fn place(v: &SpanView, scr: Screen) -> (i32, i32) {
    let (w, h) = (v.w as i32, v.h as i32);
    let ax = scr.x + (v.along * scr.width as f32).round() as i32;
    let ay = scr.y + (v.along * scr.height as f32).round() as i32;
    match v.entry {
        Edge::Left => (scr.x + v.crossed - w, ay),
        Edge::Right => (scr.x + scr.width - v.crossed, ay),
        Edge::Top => (ax, scr.y + v.crossed - h),
        Edge::Bottom => (ax, scr.y + scr.height - v.crossed),
        _ => (ax, ay),
    }
}

/// `w` x `h` scaled down to fit in `max_w` x `max_h`, keeping its shape.
pub fn fit(w: u32, h: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if w <= max_w && h <= max_h {
        return (w.max(1), h.max(1));
    }
    let k = (max_w as f64 / w as f64).min(max_h as f64 / h as f64);
    (
        ((w as f64 * k).round() as u32).max(1),
        ((h as f64 * k).round() as u32).max(1),
    )
}

/// Resizes a 4-byte-per-pixel picture (nearest pixel; it is moving anyway).
pub fn scale4(src: &[u8], sw: u32, sh: u32, dw: u32, dh: u32) -> Vec<u8> {
    let (sw, sh, dw, dh) = (sw as usize, sh as usize, dw as usize, dh as usize);
    let mut out = vec![0u8; dw * dh * 4];
    if sw == 0 || sh == 0 || src.len() < sw * sh * 4 {
        return out;
    }
    let xs: Vec<usize> = (0..dw)
        .map(|x| (x * sw / dw.max(1)).min(sw - 1) * 4)
        .collect();
    for y in 0..dh {
        let row = (y * sh / dh.max(1)).min(sh - 1) * sw * 4;
        let o = &mut out[y * dw * 4..(y + 1) * dw * 4];
        for (x, &sx) in xs.iter().enumerate() {
            o[x * 4..x * 4 + 4].copy_from_slice(&src[row + sx..row + sx + 4]);
        }
    }
    out
}

/// BGRA (any alpha) to JPEG.
pub fn encode_jpeg(bgra: &[u8], w: u32, h: u32, quality: u8) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || w > u16::MAX as u32 || h > u16::MAX as u32 {
        return None;
    }
    let mut out = Vec::with_capacity((w * h / 8) as usize);
    jpeg_encoder::Encoder::new(&mut out, quality)
        .encode(bgra, w as u16, h as u16, jpeg_encoder::ColorType::Bgra)
        .ok()?;
    Some(out)
}

/// JPEG to opaque BGRA. Refuses anything implausibly big (it comes over the network).
pub fn decode_jpeg(data: &[u8]) -> Option<(Vec<u8>, u32, u32)> {
    let mut d = jpeg_decoder::Decoder::new(data);
    d.read_info().ok()?;
    let info = d.info()?;
    let (w, h) = (info.width as u32, info.height as u32);
    if w == 0 || h == 0 || w > 4096 || h > 4096 {
        return None;
    }
    let px = d.decode().ok()?;
    let n = (w * h) as usize;
    let mut out = vec![255u8; n * 4];
    match info.pixel_format {
        jpeg_decoder::PixelFormat::RGB24 if px.len() >= n * 3 => {
            for i in 0..n {
                out[i * 4] = px[i * 3 + 2];
                out[i * 4 + 1] = px[i * 3 + 1];
                out[i * 4 + 2] = px[i * 3];
            }
        }
        jpeg_decoder::PixelFormat::L8 if px.len() >= n => {
            for i in 0..n {
                out[i * 4..i * 4 + 3].fill(px[i]);
            }
        }
        _ => return None,
    }
    Some((out, w, h))
}

/// The arrow pointer (premultiplied BGRA), its size and where its tip is.
pub fn arrow() -> (Vec<u8>, u32, u32, i32, i32) {
    // The same arrow the website draws: tip at (3, 2).
    const PTS: [(f32, f32); 7] = [
        (3.0, 2.0),
        (3.0, 24.0),
        (9.0, 18.6),
        (13.4, 28.0),
        (17.0, 26.4),
        (12.7, 17.2),
        (21.0, 17.0),
    ];
    let (w, h) = (24u32, 30u32);
    let inside = |x: f32, y: f32| {
        let mut c = false;
        let mut j = PTS.len() - 1;
        for (i, &(xi, yi)) in PTS.iter().enumerate() {
            let (xj, yj) = PTS[j];
            if (yi > y) != (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi {
                c = !c;
            }
            j = i;
        }
        c
    };
    let edge_dist = |x: f32, y: f32| {
        let mut best = f32::MAX;
        for (i, &(ax, ay)) in PTS.iter().enumerate() {
            let (bx, by) = PTS[(i + 1) % PTS.len()];
            let (dx, dy) = (bx - ax, by - ay);
            let t = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            let (px, py) = (ax + t * dx - x, ay + t * dy - y);
            best = best.min((px * px + py * py).sqrt());
        }
        best
    };
    let mut out = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let (mut cover, mut white) = (0f32, 0f32);
            for sy in 0..4 {
                for sx in 0..4 {
                    let (fx, fy) = (
                        x as f32 + (sx as f32 + 0.5) / 4.0,
                        y as f32 + (sy as f32 + 0.5) / 4.0,
                    );
                    let near = edge_dist(fx, fy) < 1.1;
                    if inside(fx, fy) || near {
                        cover += 1.0 / 16.0;
                        if inside(fx, fy) && !near {
                            white += 1.0 / 16.0;
                        }
                    }
                }
            }
            // White body, dark outline (#0a1118), premultiplied.
            let a = cover.min(1.0);
            let (r, g, b) = (
                white * 255.0 + (a - white) * 10.0,
                white * 255.0 + (a - white) * 17.0,
                white * 255.0 + (a - white) * 24.0,
            );
            let i = ((y * w + x) * 4) as usize;
            out[i] = b.round() as u8;
            out[i + 1] = g.round() as u8;
            out[i + 2] = r.round() as u8;
            out[i + 3] = (a * 255.0).round() as u8;
        }
    }
    (out, w, h, 3, 2)
}

/// What the receiving side puts on screen: the picture at the window's size (or, until
/// the first picture arrives, a dim outline) with the pointer drawn where it holds it.
/// Premultiplied BGRA, `w` x `h`.
pub fn compose(frame: Option<(&[u8], u32, u32)>, w: u32, h: u32, gx: i32, gy: i32) -> Vec<u8> {
    let mut out = match frame {
        Some((px, fw, fh)) => {
            let mut v = scale4(px, fw, fh, w, h);
            for p in v.as_chunks_mut::<4>().0 {
                p[3] = 255;
            }
            v
        }
        None => {
            let mut v = vec![0u8; (w * h * 4) as usize];
            for y in 0..h {
                for x in 0..w {
                    let border = x < 2 || y < 2 || x + 2 >= w || y + 2 >= h;
                    let (c, a) = if border {
                        (140u8, 230u8)
                    } else {
                        (24u8, 150u8)
                    };
                    let pm = (c as u32 * a as u32 / 255) as u8;
                    let i = ((y * w + x) * 4) as usize;
                    v[i..i + 4].copy_from_slice(&[pm, pm, pm, a]);
                }
            }
            v
        }
    };
    let (ar, aw, ah, tx, ty) = arrow();
    for y in 0..ah as i32 {
        for x in 0..aw as i32 {
            let (dx, dy) = (gx - tx + x, gy - ty + y);
            if dx < 0 || dy < 0 || dx >= w as i32 || dy >= h as i32 {
                continue;
            }
            let s = ((y as u32 * aw + x as u32) * 4) as usize;
            let d = ((dy as u32 * w + dx as u32) * 4) as usize;
            let a = ar[s + 3] as u32;
            for c in 0..4 {
                out[d + c] = (ar[s + c] as u32 + out[d + c] as u32 * (255 - a) / 255) as u8;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCR: Screen = Screen {
        x: 0,
        y: 0,
        width: 2560,
        height: 1440,
    };

    fn right(w: i32) -> Geo {
        Geo {
            edge: Edge::Right,
            scr: SCR,
            w,
            h: 800,
            gx: 300,
            gy: 15,
        }
    }

    #[test]
    fn a_window_pushed_through_the_right_edge() {
        let g = right(1200);
        // The pointer at the edge, holding the window 300 px from its left: 899 px are past.
        let (vx, vy) = (2559, 400);
        assert_eq!(g.crossed(vx, vy), 899);
        // 640 px past the edge (the example from the chat): left edge at 2000.
        let vx = 2000 + 300;
        assert_eq!(g.crossed(vx, vy), 640);
        assert!(g.moved(vx, vy)); // 640 of 1200 is more than half
        assert!(!g.moved(2000 - 100 + 300, vy)); // 540 of 1200
                                                 // Can't go further than all of it on the other side.
        let (cx, _) = g.clamp(99_999, vy);
        assert_eq!(g.crossed(cx, vy), 1200);
        // Back inside: the window just touches the edge.
        let (bx, _) = g.back_inside(vx, vy);
        assert_eq!(g.crossed(bx, vy), 0);
        // Along: its top as a share of the screen.
        assert!((g.along(vx, 400) - (385.0 / 1440.0)).abs() < 1e-6);
    }

    #[test]
    fn the_other_screen_shows_the_part_past_the_edge() {
        // Sender's right edge = the receiver's left; the receiver is 1920x1080.
        let other = Screen {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let v = SpanView {
            entry: Edge::Left,
            crossed: 640,
            along: 0.5,
            w: 1200,
            h: 800,
            gx: 300,
            gy: 15,
        };
        // 640 px visible from the receiver's left edge, halfway down.
        assert_eq!(place(&v, other), (640 - 1200, 540));
        let v = SpanView {
            entry: Edge::Right,
            ..v
        };
        assert_eq!(place(&v, other), (1920 - 640, 540));
        let v = SpanView {
            entry: Edge::Top,
            along: 0.25,
            ..v
        };
        assert_eq!(place(&v, other), (480, 640 - 800));
        let v = SpanView {
            entry: Edge::Bottom,
            along: 0.25,
            ..v
        };
        assert_eq!(place(&v, other), (480, 1080 - 640));
    }

    #[test]
    fn every_edge_measures_its_own_way() {
        let g = Geo {
            edge: Edge::Left,
            scr: SCR,
            w: 1000,
            h: 600,
            gx: 100,
            gy: 10,
        };
        assert_eq!(g.crossed(100, 300), 0); // window's left on the edge
        assert_eq!(g.crossed(-400 + 100, 300), 400);
        let (cx, _) = g.clamp(-99_999, 300);
        assert_eq!(g.crossed(cx, 300), 1000);
        let g = Geo {
            edge: Edge::Top,
            scr: SCR,
            w: 1000,
            h: 600,
            gx: 100,
            gy: 10,
        };
        assert_eq!(g.crossed(500, 10 - 250), 250);
        assert!(!g.moved(500, 10 - 250));
        assert!(g.moved(500, 10 - 350));
        let g = Geo {
            edge: Edge::Bottom,
            scr: SCR,
            w: 1000,
            h: 600,
            gx: 100,
            gy: 10,
        };
        assert_eq!(g.crossed(500, 1440 - 600 + 10 + 200), 200);
        let (_, by) = g.back_inside(500, 1440 + 100);
        assert_eq!(g.crossed(500, by), 0);
    }

    #[test]
    fn pictures_survive_the_trip() {
        let (w, h) = (64u32, 40u32);
        let mut px = vec![0u8; (w * h * 4) as usize];
        for (i, p) in px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
            // A blue left half and a red right half.
            let x = i as u32 % w;
            *p = if x < w / 2 {
                [200, 40, 30, 255]
            } else {
                [30, 40, 200, 255]
            };
        }
        let jpg = encode_jpeg(&px, w, h, FRAME_QUALITY).unwrap();
        let (back, bw, bh) = decode_jpeg(&jpg).unwrap();
        assert_eq!((bw, bh), (w, h));
        let at = |x: u32, y: u32| &back[((y * w + x) * 4) as usize..((y * w + x) * 4 + 4) as usize];
        assert!(at(5, 20)[0] > 150 && at(5, 20)[2] < 80, "blue stays blue");
        assert!(at(60, 20)[2] > 150 && at(60, 20)[0] < 80, "red stays red");
        assert!(decode_jpeg(b"not a picture").is_none());
    }

    #[test]
    fn scaling_and_fitting() {
        assert_eq!(fit(2560, 1440, FRAME_MAX_W, FRAME_MAX_H), (1280, 720));
        assert_eq!(fit(800, 600, FRAME_MAX_W, FRAME_MAX_H), (800, 600));
        assert_eq!(fit(1000, 3000, FRAME_MAX_W, FRAME_MAX_H), (300, 900));
        let src = [1, 2, 3, 4, 5, 6, 7, 8]; // 2x1
        assert_eq!(
            scale4(&src, 2, 1, 4, 1),
            vec![1, 2, 3, 4, 1, 2, 3, 4, 5, 6, 7, 8, 5, 6, 7, 8]
        );
    }

    #[test]
    fn the_pointer_is_drawn_where_the_window_is_held() {
        let out = compose(None, 100, 80, 50, 40);
        // The arrow's white body just below and right of its tip.
        let i = ((46 * 100 + 52) * 4) as usize;
        assert!(
            out[i] > 200 && out[i + 3] == 255,
            "white pointer body at the hold point"
        );
        // Far from it: the dim outline / inside.
        assert!(out[((70 * 100 + 10) * 4) as usize + 3] < 255);
        let (a, w, h, tx, ty) = arrow();
        assert_eq!((w, h, tx, ty), (24, 30, 3, 2));
        assert_eq!(a[3], 0, "corner is transparent");
    }
}
