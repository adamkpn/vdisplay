//! Geometry for `--view`: screen areas (halves, thirds, quarters), fitting the
//! viewer window into one, and mapping the pointer between the viewer and the
//! virtual display. All rects are global CoreGraphics coordinates in points.

use crate::ffi::{CGPoint, CGRect, CGSize};
use clap::ValueEnum;

#[derive(Clone, Copy, Debug, PartialEq, ValueEnum)]
pub enum Area {
    /// Left half
    Left,
    /// Right half
    Right,
    /// Left third
    LeftThird,
    /// Middle third
    CenterThird,
    /// Right third
    RightThird,
    /// Top-left quarter
    TopLeft,
    /// Top-right quarter
    TopRight,
    /// Bottom-left quarter
    BottomLeft,
    /// Bottom-right quarter
    BottomRight,
}

impl Area {
    /// CLI name, e.g. `left-third`.
    pub fn name(self) -> String {
        self.to_possible_value()
            .map(|v| v.get_name().to_string())
            .unwrap_or_default()
    }

    /// Grid cell: (column, row, columns, rows).
    fn cell(self) -> (u32, u32, u32, u32) {
        use Area::*;
        match self {
            Left => (0, 0, 2, 1),
            Right => (1, 0, 2, 1),
            LeftThird => (0, 0, 3, 1),
            CenterThird => (1, 0, 3, 1),
            RightThird => (2, 0, 3, 1),
            TopLeft => (0, 0, 2, 2),
            TopRight => (1, 0, 2, 2),
            BottomLeft => (0, 1, 2, 2),
            BottomRight => (1, 1, 2, 2),
        }
    }

    /// The part of `screen` this area covers. Edges snap to whole points, and
    /// neighbouring areas share an edge, so they tile without gaps.
    pub fn rect(self, screen: CGRect) -> CGRect {
        let (col, row, cols, rows) = self.cell();
        let edge =
            |start: f64, len: f64, i: u32, n: u32| (start + len * i as f64 / n as f64).round();
        let (o, s) = (screen.origin, screen.size);
        let (x0, x1) = (
            edge(o.x, s.width, col, cols),
            edge(o.x, s.width, col + 1, cols),
        );
        let (y0, y1) = (
            edge(o.y, s.height, row, rows),
            edge(o.y, s.height, row + 1, rows),
        );
        CGRect {
            origin: CGPoint { x: x0, y: y0 },
            size: CGSize {
                width: x1 - x0,
                height: y1 - y0,
            },
        }
    }
}

/// Refresh rate for the virtual display: `--refresh` if given, else the rate
/// of the screen the viewer is on, so every captured frame lines up with one
/// of its refreshes (60 fps on an 85 Hz panel stutters), else 60 Hz.
pub fn refresh_rate(arg: Option<f64>, view_hz: Option<f64>) -> f64 {
    arg.or(view_hz.filter(|hz| (24.0..=480.0).contains(hz)))
        .unwrap_or(60.0)
}

/// `v` rounded down to an even number (some GPUs reject odd display sizes).
pub fn even_floor(v: f64) -> u32 {
    (v.max(0.0) as u32) & !1
}

/// The largest rect with aspect ratio `w`:`h` that fits in `area`, centered,
/// on whole points.
pub fn fit(area: CGRect, w: f64, h: f64) -> CGRect {
    // The side that fills the area keeps its exact length.
    let size = if area.size.width / w <= area.size.height / h {
        CGSize {
            width: area.size.width,
            height: (h * area.size.width / w).floor(),
        }
    } else {
        CGSize {
            width: (w * area.size.height / h).floor(),
            height: area.size.height,
        }
    };
    CGRect {
        origin: CGPoint {
            x: (area.origin.x + (area.size.width - size.width) / 2.0).round(),
            y: (area.origin.y + (area.size.height - size.height) / 2.0).round(),
        },
        size,
    }
}

/// Maps `p` from `from` to the same relative spot in `to`, kept inside `to`.
pub fn map_point(from: CGRect, to: CGRect, p: CGPoint) -> CGPoint {
    let rel = |v: f64, o: f64, len: f64| ((v - o) / len).clamp(0.0, 1.0);
    let (fo, fs, to_o, to_s) = (from.origin, from.size, to.origin, to.size);
    CGPoint {
        x: (to_o.x + rel(p.x, fo.x, fs.width) * to_s.width).min(to_o.x + to_s.width - 1.0),
        y: (to_o.y + rel(p.y, fo.y, fs.height) * to_s.height).min(to_o.y + to_s.height - 1.0),
    }
}

/// Where the real pointer is, and its last known position.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pointer {
    /// On a real screen.
    Outside(CGPoint),
    /// Moved onto the virtual display because it entered the viewer.
    Inside(CGPoint),
}

/// Entering the viewer lands the pointer this far inside the virtual
/// display's exit edges. Landing right on one (entering from the right puts
/// it on the right edge) made the next tick push it straight back out.
pub const ENTRY_INSET: f64 = 2.0;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Side {
    Left,
    Right,
    Bottom,
}

/// `p` moved into `r` (onto its last whole point on each side).
fn clamp_into(r: CGRect, p: CGPoint) -> CGPoint {
    let (o, s) = (r.origin, r.size);
    CGPoint {
        x: p.x.max(o.x).min(o.x + s.width - 1.0),
        y: p.y.max(o.y).min(o.y + s.height - 1.0),
    }
}

/// Where on the virtual display `virt` a pointer at `p` in the viewer goes.
pub fn entry_point(view: CGRect, virt: CGRect, p: CGPoint) -> CGPoint {
    let v = map_point(view, virt, p);
    let (o, s) = (virt.origin, virt.size);
    CGPoint {
        x: v.x
            .max(o.x + ENTRY_INSET)
            .min(o.x + s.width - 1.0 - ENTRY_INSET),
        y: v.y.min(o.y + s.height - 1.0 - ENTRY_INSET),
    }
}

/// The exit edge `p` (on `virt`) is pushed against. The top is not one, so
/// the full-screen toolbar there stays reachable.
fn pinned_side(virt: CGRect, p: CGPoint) -> Option<Side> {
    let (o, s) = (virt.origin, virt.size);
    if p.x - o.x < 1.0 {
        Some(Side::Left)
    } else if o.x + s.width - 1.0 - p.x < 1.0 {
        Some(Side::Right)
    } else if o.y + s.height - 1.0 - p.y < 1.0 {
        Some(Side::Bottom)
    } else {
        None
    }
}

/// The side of `virt` that `p` (just off it, on a neighbouring display)
/// crossed; None for the top.
fn crossed_side(virt: CGRect, p: CGPoint) -> Option<Side> {
    let (o, s) = (virt.origin, virt.size);
    if p.x < o.x {
        Some(Side::Left)
    } else if p.x >= o.x + s.width {
        Some(Side::Right)
    } else if p.y >= o.y + s.height {
        Some(Side::Bottom)
    } else {
        None
    }
}

/// Just outside the viewer's matching edge, for a pointer leaving `virt` at
/// `at` by `side`; None if no real screen is there.
fn leave(
    view: CGRect,
    virt: CGRect,
    at: CGPoint,
    side: Side,
    screens: &[CGRect],
) -> Option<CGPoint> {
    let q = map_point(virt, view, at);
    let (x0, x1, y1) = (
        view.origin.x,
        view.origin.x + view.size.width,
        view.origin.y + view.size.height,
    );
    let t = match side {
        Side::Left => CGPoint {
            x: x0 - 2.0,
            y: q.y,
        },
        Side::Right => CGPoint {
            x: x1 + 1.0,
            y: q.y,
        },
        Side::Bottom => CGPoint {
            x: q.x,
            y: y1 + 1.0,
        },
    };
    (screens.iter().any(|s| s.contains(t)) && !view.contains(t)).then_some(t)
}

/// One step of the pointer handoff: with the pointer now at `p`, the new
/// state and where to move the pointer, if anywhere. `screens` are the real
/// displays; `capturing` is false while the viewer shows no picture.
pub fn step(
    state: Pointer,
    p: CGPoint,
    view: CGRect,
    virt: CGRect,
    screens: &[CGRect],
    capturing: bool,
) -> (Pointer, Option<CGPoint>) {
    match state {
        // Reached the virtual display across a shared edge, not through the
        // viewer: hold it at that edge (going back to the last position
        // made it bounce).
        Pointer::Outside(last) if virt.contains(p) => {
            let back = screens
                .iter()
                .find(|s| s.contains(last))
                .map_or(last, |s| clamp_into(*s, p));
            (Pointer::Outside(back), Some(back))
        }
        Pointer::Outside(_) if capturing && view.contains(p) => {
            let v = entry_point(view, virt, p);
            (Pointer::Inside(v), Some(v))
        }
        Pointer::Outside(_) => (Pointer::Outside(p), None),
        // Pushed against an edge with a real screen beyond the viewer: leave.
        Pointer::Inside(_) if virt.contains(p) => {
            match pinned_side(virt, p).and_then(|side| leave(view, virt, p, side, screens)) {
                Some(out) => (Pointer::Outside(out), Some(out)),
                None => (Pointer::Inside(p), None),
            }
        }
        // Crossed into a neighbouring display: leave by the side it crossed,
        // or hold it at that edge if there is nowhere to go.
        Pointer::Inside(_) => {
            let edge = clamp_into(virt, p);
            match crossed_side(virt, p).and_then(|side| leave(view, virt, edge, side, screens)) {
                Some(out) => (Pointer::Outside(out), Some(out)),
                None => (Pointer::Inside(edge), Some(edge)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refresh_follows_the_viewer_screen() {
        assert_eq!(refresh_rate(None, Some(85.0)), 85.0);
        assert_eq!(refresh_rate(Some(60.0), Some(85.0)), 60.0);
        assert_eq!(refresh_rate(None, None), 60.0);
        // Some displays report 0 Hz (unknown).
        assert_eq!(refresh_rate(None, Some(0.0)), 60.0);
    }

    #[test]
    fn even_floor_rounds_down() {
        assert_eq!(even_floor(1409.0), 1408);
        assert_eq!(even_floor(1720.0), 1720);
        assert_eq!(even_floor(-3.0), 0);
    }

    #[test]
    fn fit_keeps_aspect_and_centers() {
        let area = rect(0.0, 31.0, 1720.0, 1409.0);
        assert_eq!(fit(area, 1720.0, 1408.0), rect(0.0, 32.0, 1720.0, 1408.0));
        // 16:9 into the half: full width, centered vertically.
        assert_eq!(fit(area, 1920.0, 1080.0), rect(0.0, 252.0, 1720.0, 967.0));
    }

    const VIEW: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 32.0 },
        size: CGSize {
            width: 1720.0,
            height: 1408.0,
        },
    };
    // Virtual display arranged to the right of a 3440 pt wide main display.
    const VIRT: CGRect = CGRect {
        origin: CGPoint { x: 3440.0, y: 0.0 },
        size: CGSize {
            width: 1720.0,
            height: 1408.0,
        },
    };

    fn assert_near(a: CGPoint, b: CGPoint) {
        assert!(
            (a.x - b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6,
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn maps_view_to_virtual_and_back() {
        let p = CGPoint { x: 100.0, y: 132.0 };
        let v = map_point(VIEW, VIRT, p);
        assert_near(
            v,
            CGPoint {
                x: 3540.0,
                y: 100.0,
            },
        );
        assert_near(map_point(VIRT, VIEW, v), p);
    }

    #[test]
    fn map_stays_inside_the_target() {
        let v = map_point(
            VIEW,
            VIRT,
            CGPoint {
                x: 5000.0,
                y: -50.0,
            },
        );
        assert_eq!(
            v,
            CGPoint {
                x: 3440.0 + 1719.0,
                y: 0.0
            }
        );
    }

    /// The 3440x1440 main display the viewer and `VIRT` sit on / next to.
    const MAIN: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 0.0 },
        size: CGSize {
            width: 3440.0,
            height: 1440.0,
        },
    };

    fn step_main(state: Pointer, p: CGPoint) -> (Pointer, Option<CGPoint>) {
        step(state, p, VIEW, VIRT, &[MAIN], true)
    }

    #[test]
    fn entering_from_the_right_is_not_pushed_straight_back_out() {
        let at_edge = CGPoint {
            x: 1719.0,
            y: 732.0,
        };
        let (state, warp) = step_main(
            Pointer::Outside(CGPoint {
                x: 1725.0,
                y: 732.0,
            }),
            at_edge,
        );
        let Pointer::Inside(v) = state else {
            panic!("should enter: {state:?}")
        };
        assert_eq!(warp, Some(v));
        assert_eq!(v.x, 3440.0 + 1719.0 - ENTRY_INSET);
        // The pointer hasn't moved yet: it stays in.
        assert_eq!(step_main(state, v), (Pointer::Inside(v), None));
    }

    #[test]
    fn leaves_right_of_the_view_when_pushed_against_the_right_edge() {
        let p = CGPoint {
            x: 3440.0 + 1719.0,
            y: 700.0,
        };
        let out = CGPoint {
            x: 1721.0,
            y: 732.0,
        };
        assert_eq!(
            step_main(Pointer::Inside(p), p),
            (Pointer::Outside(out), Some(out))
        );
    }

    #[test]
    fn stays_inside_away_from_edges_and_at_the_top() {
        for p in [
            CGPoint {
                x: 4000.0,
                y: 700.0,
            },
            CGPoint { x: 4000.0, y: 0.0 },
        ] {
            assert_eq!(step_main(Pointer::Inside(p), p), (Pointer::Inside(p), None));
        }
        // Crossing the top (into a display above) is held at the edge.
        let above = CGPoint { x: 4000.0, y: -3.0 };
        let edge = CGPoint { x: 4000.0, y: 0.0 };
        assert_eq!(
            step_main(Pointer::Inside(edge), above),
            (Pointer::Inside(edge), Some(edge))
        );
    }

    #[test]
    fn crossing_onto_the_virtual_display_is_held_at_the_edge() {
        // From the right edge of the main display into VIRT next to it.
        let last = CGPoint {
            x: 3400.0,
            y: 700.0,
        };
        let p = CGPoint {
            x: 3460.0,
            y: 700.0,
        };
        let edge = CGPoint {
            x: 3439.0,
            y: 700.0,
        };
        assert_eq!(
            step_main(Pointer::Outside(last), p),
            (Pointer::Outside(edge), Some(edge))
        );
    }

    #[test]
    fn crossing_into_a_neighbour_leaves_by_that_side() {
        // VIRT's left edge touches the main display. With the view in the
        // right half there is room left of it.
        let view = rect(1720.0, 32.0, 1720.0, 1408.0);
        let inside = CGPoint {
            x: 3445.0,
            y: 700.0,
        };
        let crossed = CGPoint {
            x: 3430.0,
            y: 700.0,
        };
        let out = CGPoint {
            x: 1718.0,
            y: 732.0,
        };
        assert_eq!(
            step(Pointer::Inside(inside), crossed, view, VIRT, &[MAIN], true),
            (Pointer::Outside(out), Some(out))
        );
        // With the view at the screen's left edge there is not: held at the edge.
        let edge = CGPoint {
            x: 3440.0,
            y: 700.0,
        };
        assert_eq!(
            step_main(Pointer::Inside(inside), crossed),
            (Pointer::Inside(edge), Some(edge))
        );
    }

    #[test]
    fn no_handoff_without_a_picture() {
        let p = CGPoint { x: 100.0, y: 300.0 };
        let state = Pointer::Outside(CGPoint {
            x: 1800.0,
            y: 300.0,
        });
        assert_eq!(
            step(state, p, VIEW, VIRT, &[MAIN], false),
            (Pointer::Outside(p), None)
        );
    }

    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize {
                width: w,
                height: h,
            },
        }
    }

    // 3440x1440 ultrawide with a 25 pt menu bar and the Dock hidden.
    const SCREEN: CGRect = CGRect {
        origin: CGPoint { x: 0.0, y: 25.0 },
        size: CGSize {
            width: 3440.0,
            height: 1415.0,
        },
    };

    #[test]
    fn halves() {
        assert_eq!(Area::Left.rect(SCREEN), rect(0.0, 25.0, 1720.0, 1415.0));
        assert_eq!(Area::Right.rect(SCREEN), rect(1720.0, 25.0, 1720.0, 1415.0));
    }

    #[test]
    fn thirds_tile_without_gaps() {
        let [l, c, r] =
            [Area::LeftThird, Area::CenterThird, Area::RightThird].map(|a| a.rect(SCREEN));
        assert_eq!(l.origin.x, 0.0);
        assert_eq!(l.origin.x + l.size.width, c.origin.x);
        assert_eq!(c.origin.x + c.size.width, r.origin.x);
        assert_eq!(r.origin.x + r.size.width, 3440.0);
    }

    #[test]
    fn quarters() {
        assert_eq!(Area::TopLeft.rect(SCREEN), rect(0.0, 25.0, 1720.0, 708.0));
        assert_eq!(
            Area::BottomRight.rect(SCREEN),
            rect(1720.0, 733.0, 1720.0, 707.0)
        );
    }

    #[test]
    fn secondary_screen_left_of_main() {
        let screen = rect(-1920.0, 0.0, 1920.0, 1080.0);
        assert_eq!(Area::Right.rect(screen), rect(-960.0, 0.0, 960.0, 1080.0));
    }

    #[test]
    fn cli_names() {
        assert_eq!(Area::LeftThird.name(), "left-third");
        assert_eq!(Area::from_str("bottom-right", false), Ok(Area::BottomRight));
    }
}
