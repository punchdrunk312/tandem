//! Second-monitor mode: the maths both ends share, kept free of any OS so it can be
//! tested anywhere.
//!
//! A window dragged across in this mode stays on the computer it belongs to (the owner)
//! and is shown live on the other one (the mirror). On the owner it waits "parked" just
//! past the edge that leads to the mirror, with one column of pixels still on screen:
//! browsers and other apps stop drawing windows that are entirely off screen. When the
//! pointer goes onto the mirror, the owner slides the real window onto its own screen
//! (nobody is looking at that screen then) and takes the pointer, so every click and key
//! reaches the real program the normal way, whatever program it is.

use crate::platform::Screen;
use crate::proto::Edge;

/// A rectangle: left, top, width, height.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

/// How much of a parked window stays on screen.
pub const PARK_VISIBLE: i32 = 1;
/// A window slid in for the pointer stays this far from every edge of its monitor, so
/// the pointer always leaves the window before it can reach a screen edge.
pub const IN_MARGIN: i32 = 2;
/// Frames of a mirrored window: at most this big, this good, this often.
pub const FRAME_MAX_W: u32 = 2560;
pub const FRAME_MAX_H: u32 = 1600;
pub const FRAME_QUALITY: u8 = 80;
pub const FRAME_EVERY_MS: u64 = 40;
/// How often the pointer's position inside the window is sent.
pub const CURSOR_EVERY_MS: u64 = 8;

/// Where a window of visible size `w` x `h` (top-left `top`) waits on the owner: past
/// `edge` of the whole desktop `scr`, with `PARK_VISIBLE` pixels still on screen.
/// Returns the visible part's new top-left.
pub fn park_at(scr: Screen, edge: Edge, w: i32, h: i32, top: (i32, i32)) -> (i32, i32) {
    let clamp_y = |y: i32| y.clamp(scr.y, (scr.y + scr.height - h).max(scr.y));
    let clamp_x = |x: i32| x.clamp(scr.x, (scr.x + scr.width - w).max(scr.x));
    match edge {
        Edge::Right | Edge::TopRight | Edge::BottomRight => {
            (scr.x + scr.width - PARK_VISIBLE, clamp_y(top.1))
        }
        Edge::Left | Edge::TopLeft | Edge::BottomLeft => (scr.x - w + PARK_VISIBLE, clamp_y(top.1)),
        Edge::Bottom => (clamp_x(top.0), scr.y + scr.height - PARK_VISIBLE),
        Edge::Top => (clamp_x(top.0), scr.y - h + PARK_VISIBLE),
    }
}

/// Where the window goes while the pointer is on it: centred on the monitor `work` area,
/// at most the size that fits with `IN_MARGIN` all round. Returns the visible part's
/// rectangle (smaller than `w` x `h` if the window had to shrink).
pub fn slide_in(work: Rect, w: i32, h: i32) -> Rect {
    let mw = (work.w - 2 * IN_MARGIN).max(40);
    let mh = (work.h - 2 * IN_MARGIN).max(30);
    let (w, h) = (w.min(mw), h.min(mh));
    Rect {
        x: work.x + (work.w - w) / 2,
        y: work.y + (work.h - h) / 2,
        w,
        h,
    }
}

/// The mirror was dragged to `m` on the mirror's screen `scr`, into which the window came
/// through `entry`. Past that edge by more than half its size: it goes home, at the
/// returned fraction along the edge.
pub fn home_along(m: Rect, scr: Screen, entry: Edge) -> Option<f32> {
    let frac_y = |y: i32| ((y - scr.y) as f32 / scr.height.max(1) as f32).clamp(0.0, 1.0);
    let frac_x = |x: i32| ((x - scr.x) as f32 / scr.width.max(1) as f32).clamp(0.0, 1.0);
    let past = match entry {
        Edge::Left | Edge::TopLeft | Edge::BottomLeft => scr.x - m.x,
        Edge::Right | Edge::TopRight | Edge::BottomRight => m.x + m.w - (scr.x + scr.width),
        Edge::Top => scr.y - m.y,
        Edge::Bottom => m.y + m.h - (scr.y + scr.height),
    };
    let size = if matches!(entry, Edge::Top | Edge::Bottom) {
        m.h
    } else {
        m.w
    };
    if past * 2 <= size {
        return None;
    }
    Some(match entry {
        Edge::Top | Edge::Bottom => frac_x(m.x),
        _ => frac_y(m.y),
    })
}

/// The window comes home through `edge` of the owner's desktop `scr` at `along`: where
/// its visible part goes, fully on screen against that edge.
pub fn home_at(scr: Screen, edge: Edge, along: f32, w: i32, h: i32) -> (i32, i32) {
    let along = along.clamp(0.0, 1.0);
    let y = (scr.y + (along * scr.height as f32) as i32)
        .clamp(scr.y, (scr.y + scr.height - h).max(scr.y));
    let x = (scr.x + (along * scr.width as f32) as i32)
        .clamp(scr.x, (scr.x + scr.width - w).max(scr.x));
    match edge {
        Edge::Right | Edge::TopRight | Edge::BottomRight => ((scr.x + scr.width - w).max(scr.x), y),
        Edge::Left | Edge::TopLeft | Edge::BottomLeft => (scr.x, y),
        Edge::Bottom => (x, (scr.y + scr.height - h).max(scr.y)),
        Edge::Top => (x, scr.y),
    }
}

/// The mirror's first place: where the picture of the window being dragged was when it
/// was let go, pulled fully onto the screen.
pub fn first_place(pic: Rect, scr: Screen) -> Rect {
    let x = pic.x.clamp(scr.x, (scr.x + scr.width - pic.w).max(scr.x));
    let y = pic.y.clamp(scr.y, (scr.y + scr.height - pic.h).max(scr.y));
    Rect { x, y, ..pic }
}

/// A cheap fingerprint of a picture, to skip sending frames that did not change.
pub fn fingerprint(px: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    // Every 7th pixel, every row: catches a blinking caret, costs little.
    for chunk in px.chunks(4).step_by(7) {
        for &b in chunk {
            h ^= b as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    h ^ px.len() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCR: Screen = Screen {
        x: 0,
        y: 0,
        width: 1920,
        height: 1080,
    };

    #[test]
    fn a_parked_window_keeps_one_column_on_screen() {
        assert_eq!(park_at(SCR, Edge::Right, 800, 600, (500, 100)), (1919, 100));
        assert_eq!(park_at(SCR, Edge::Left, 800, 600, (500, 100)), (-799, 100));
        assert_eq!(
            park_at(SCR, Edge::Bottom, 800, 600, (500, 100)),
            (500, 1079)
        );
        assert_eq!(park_at(SCR, Edge::Top, 800, 600, (500, 100)), (500, -599));
        // Kept within the screen along the edge.
        assert_eq!(park_at(SCR, Edge::Right, 800, 600, (0, 900)), (1919, 480));
    }

    #[test]
    fn sliding_in_centres_and_shrinks_to_fit() {
        let work = Rect {
            x: 0,
            y: 0,
            w: 1920,
            h: 1040,
        };
        assert_eq!(
            slide_in(work, 800, 600),
            Rect {
                x: 560,
                y: 220,
                w: 800,
                h: 600
            }
        );
        let big = slide_in(work, 2560, 1440);
        assert_eq!((big.w, big.h), (1916, 1036));
        assert_eq!((big.x, big.y), (2, 2));
    }

    #[test]
    fn dragging_the_mirror_back_over_the_edge_brings_it_home() {
        // Came in through the left edge of the mirror's screen.
        let m = |x| Rect {
            x,
            y: 540,
            w: 800,
            h: 600,
        };
        assert_eq!(home_along(m(100), SCR, Edge::Left), None);
        assert_eq!(home_along(m(-300), SCR, Edge::Left), None);
        let a = home_along(m(-500), SCR, Edge::Left).unwrap();
        assert!((a - 0.5).abs() < 1e-6);
        // Through the right edge.
        assert!(home_along(m(1600), SCR, Edge::Right).is_some());
        assert!(home_along(m(1200), SCR, Edge::Right).is_none());
    }

    #[test]
    fn coming_home_lands_against_the_edge() {
        assert_eq!(home_at(SCR, Edge::Right, 0.5, 800, 600), (1120, 480));
        assert_eq!(home_at(SCR, Edge::Left, 0.1, 800, 600), (0, 108));
        assert_eq!(home_at(SCR, Edge::Bottom, 0.0, 800, 600), (0, 480));
    }

    #[test]
    fn the_first_place_is_fully_on_screen() {
        let p = first_place(
            Rect {
                x: -300,
                y: 900,
                w: 800,
                h: 600,
            },
            SCR,
        );
        assert_eq!((p.x, p.y), (0, 480));
    }

    #[test]
    fn fingerprints_notice_small_changes() {
        let mut a = vec![10u8; 4 * 1000];
        let fa = fingerprint(&a);
        a[0] = 11;
        assert_ne!(fa, fingerprint(&a));
        assert_eq!(fingerprint(&a), fingerprint(&a.clone()));
    }
}
