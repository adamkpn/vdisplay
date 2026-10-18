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

/// Where the pointer goes when it leaves the virtual display `virt` at `p`:
/// just outside the matching edge of the viewer `view`. Leaving through the
/// top never exits, so the full-screen toolbar at the top edge stays reachable.
///
/// With `at_edge_only`, `p` must be on the left, right or bottom edge (the
/// pointer is pinned there); otherwise the nearest edge is used (the pointer
/// already crossed into a neighbouring display from `p`).
pub fn exit_point(view: CGRect, virt: CGRect, p: CGPoint, at_edge_only: bool) -> Option<CGPoint> {
    let (vo, vs) = (virt.origin, virt.size);
    let left = p.x - vo.x;
    let right = vo.x + vs.width - 1.0 - p.x;
    let bottom = vo.y + vs.height - 1.0 - p.y;
    let top = p.y - vo.y;
    let nearest = left.min(right).min(bottom);
    if (at_edge_only && nearest >= 1.0) || (!at_edge_only && top < nearest) {
        return None;
    }
    let q = map_point(virt, view, p);
    let (x0, x1, y1) = (
        view.origin.x,
        view.origin.x + view.size.width,
        view.origin.y + view.size.height,
    );
    Some(if nearest == left {
        CGPoint {
            x: x0 - 2.0,
            y: q.y,
        }
    } else if nearest == right {
        CGPoint {
            x: x1 + 1.0,
            y: q.y,
        }
    } else {
        CGPoint {
            x: q.x,
            y: y1 + 1.0,
        }
    })
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

    #[test]
    fn exits_right_of_the_view_at_the_right_edge() {
        let p = CGPoint {
            x: 3440.0 + 1719.0,
            y: 700.0,
        };
        let out = exit_point(VIEW, VIRT, p, true).expect("should exit");
        assert_near(
            out,
            CGPoint {
                x: 1721.0,
                y: 732.0,
            },
        );
    }

    #[test]
    fn stays_inside_away_from_edges_and_at_the_top() {
        let middle = CGPoint {
            x: 4000.0,
            y: 700.0,
        };
        assert_eq!(exit_point(VIEW, VIRT, middle, true), None);
        let top = CGPoint { x: 4000.0, y: 0.0 };
        assert_eq!(exit_point(VIEW, VIRT, top, true), None);
        assert_eq!(exit_point(VIEW, VIRT, top, false), None);
    }

    #[test]
    fn crossing_the_left_edge_targets_left_of_the_view() {
        let p = CGPoint {
            x: 3441.0,
            y: 700.0,
        };
        let out = exit_point(VIEW, VIRT, p, false).expect("should exit");
        assert_near(out, CGPoint { x: -2.0, y: 732.0 });
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
