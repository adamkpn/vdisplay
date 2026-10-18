//! Virtual display sizes derived from a physical display's native resolution.
//! `suggestions` keep the panel's aspect ratio, so `--mirror` fills the
//! screen without black bars; `regions` match a part of the panel instead.

/// Smallest and largest sides `--resolution` accepts, in points.
pub const MIN_WIDTH: u32 = 320;
pub const MIN_HEIGHT: u32 = 240;
pub const MAX_SIDE: u32 = 16384;
/// Rendered pixel count above 8K, where `applySettings` starts failing on some GPUs.
pub const LARGE_PIXELS: u64 = 7680 * 4320;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Suggestion {
    /// Points the app sees as the screen size.
    pub width: u32,
    pub height: u32,
    pub hidpi: bool,
    pub note: &'static str,
}

impl Suggestion {
    /// Pixels the virtual display renders.
    pub fn pixels(&self) -> (u32, u32) {
        let s = if self.hidpi { 2 } else { 1 };
        (self.width * s, self.height * s)
    }

    pub fn is_large(&self) -> bool {
        let (w, h) = self.pixels();
        w as u64 * h as u64 > LARGE_PIXELS
    }
}

/// A part of the panel, e.g. its left half.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub name: &'static str,
    /// Where on the panel a region of this size sits, as `--view` areas.
    pub positions: &'static str,
    pub width: u32,
    pub height: u32,
}

/// `v` scaled by `scale`, rounded to an even number: some GPUs reject odd widths.
fn even(v: u32, scale: f64) -> u32 {
    ((v as f64 * scale / 2.0).round() as u32) * 2
}

/// Suggestions for a panel of `native_w` x `native_h` pixels, smallest first.
pub fn suggestions(native_w: u32, native_h: u32) -> Vec<Suggestion> {
    let at = |scale: f64, hidpi: bool, note| Suggestion {
        width: even(native_w, scale),
        height: even(native_h, scale),
        hidpi,
        note,
    };
    [
        at(1.0, false, "1:1 with the panel when mirrored"),
        at(
            1.0,
            true,
            "same space, rendered at 2x: sharper text when mirrored",
        ),
        at(1.5, false, "1.5x the space, downscaled when mirrored"),
        at(2.0, false, "2x the space, small UI when mirrored"),
    ]
    .into_iter()
    .filter(|s| s.width >= 2 && s.width <= MAX_SIDE && s.height <= MAX_SIDE)
    .collect()
}

/// Half, third and quarter of a panel of `native_w` x `native_h` pixels,
/// largest first; regions below the minimum `--resolution` are dropped.
pub fn regions(native_w: u32, native_h: u32) -> Vec<Region> {
    let part = |name, positions, cols: u32, rows: u32| Region {
        name,
        positions,
        width: even(native_w, 1.0 / cols as f64),
        height: even(native_h, 1.0 / rows as f64),
    };
    [
        part("half", "left, right", 2, 1),
        part("third", "left-third, center-third, right-third", 3, 1),
        part(
            "quarter",
            "top-left, top-right, bottom-left, bottom-right",
            2,
            2,
        ),
    ]
    .into_iter()
    .filter(|r| r.width >= MIN_WIDTH && r.height >= MIN_HEIGHT)
    .collect()
}

/// Whether two sizes share an aspect ratio (within 1%).
pub fn same_aspect(a: (f64, f64), b: (f64, f64)) -> bool {
    let (ra, rb) = (a.0 / a.1, b.0 / b.1);
    (ra - rb).abs() / rb <= 0.01
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sizes(v: &[Suggestion]) -> Vec<(u32, u32, bool)> {
        v.iter().map(|s| (s.width, s.height, s.hidpi)).collect()
    }

    #[test]
    fn ultrawide_panel() {
        assert_eq!(
            sizes(&suggestions(3440, 1440)),
            [
                (3440, 1440, false),
                (3440, 1440, true),
                (5160, 2160, false),
                (6880, 2880, false)
            ]
        );
    }

    #[test]
    fn retina_laptop_panel_keeps_even_sizes_and_aspect() {
        for s in suggestions(3024, 1964) {
            assert_eq!((s.width % 2, s.height % 2), (0, 0), "{s:?}");
            assert!(same_aspect(
                (s.width as f64, s.height as f64),
                (3024.0, 1964.0)
            ));
        }
    }

    #[test]
    fn drops_sizes_beyond_the_resolution_limit() {
        // 1.5x = 15000 still fits; 2x = 20000 does not.
        let s = suggestions(10000, 5000);
        assert!(s.iter().all(|s| s.width <= MAX_SIDE));
        assert_eq!(s.len(), 3);
    }

    #[test]
    fn flags_sizes_above_8k() {
        let s = suggestions(5120, 2880);
        assert!(!s[0].is_large());
        assert!(s[1].is_large()); // 10240x5760 px with --hidpi
    }

    #[test]
    fn ultrawide_regions() {
        let r: Vec<_> = regions(3440, 1440)
            .iter()
            .map(|r| (r.name, r.width, r.height))
            .collect();
        assert_eq!(
            r,
            [
                ("half", 1720, 1440),
                ("third", 1146, 1440),
                ("quarter", 1720, 720)
            ]
        );
    }

    #[test]
    fn drops_regions_below_the_minimum_size() {
        // A third of 800 (266) is narrower than the 320 minimum.
        let names: Vec<_> = regions(800, 600).iter().map(|r| r.name).collect();
        assert_eq!(names, ["half", "quarter"]);
    }

    #[test]
    fn aspect_tolerance() {
        assert!(same_aspect((3840.0, 2160.0), (1920.0, 1080.0)));
        assert!(!same_aspect((3440.0, 1440.0), (3840.0, 2160.0)));
    }
}
