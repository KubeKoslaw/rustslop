//! Pure tiling layout math (no Wayland state), so it can be unit-tested.

use smithay::utils::{Logical, Rectangle};

pub type Rect = Rectangle<i32, Logical>;

/// Hyprland-style "dwindle" layout.
///
/// Window 0 gets half of `area`, window 1 half of what remains (split direction alternates
/// with the shape of the remaining space), and so on; the last window takes the rest.
/// `gap` is the space between windows and between windows and the screen edge.
/// Returned rectangles are in the same order as the windows.
pub fn dwindle(area: Rect, count: usize, gap: i32) -> Vec<Rect> {
    if count == 0 {
        return Vec::new();
    }
    let mut remaining = shrink(area, gap);
    let mut out = Vec::with_capacity(count);

    for i in 0..count {
        if i == count - 1 {
            out.push(remaining);
            break;
        }
        let (first, rest) = if remaining.size.w >= remaining.size.h {
            let w = (remaining.size.w - gap) / 2;
            (
                Rect::new(remaining.loc, (w, remaining.size.h).into()),
                Rect::new(
                    (remaining.loc.x + w + gap, remaining.loc.y).into(),
                    (remaining.size.w - w - gap, remaining.size.h).into(),
                ),
            )
        } else {
            let h = (remaining.size.h - gap) / 2;
            (
                Rect::new(remaining.loc, (remaining.size.w, h).into()),
                Rect::new(
                    (remaining.loc.x, remaining.loc.y + h + gap).into(),
                    (remaining.size.w, remaining.size.h - h - gap).into(),
                ),
            )
        };
        out.push(first);
        remaining = rest;
    }
    out
}

/// Default rectangle for a window that just became floating: centred, 60% of `area`.
pub fn default_float_rect(area: Rect) -> Rect {
    let w = area.size.w * 6 / 10;
    let h = area.size.h * 6 / 10;
    Rect::new((area.loc.x + (area.size.w - w) / 2, area.loc.y + (area.size.h - h) / 2).into(), (w, h).into())
}

fn shrink(r: Rect, by: i32) -> Rect {
    Rect::new((r.loc.x + by, r.loc.y + by).into(), ((r.size.w - 2 * by).max(1), (r.size.h - 2 * by).max(1)).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::new((0, 0).into(), (1000, 600).into())
    }

    #[test]
    fn empty_and_single() {
        assert!(dwindle(area(), 0, 10).is_empty());
        assert_eq!(dwindle(area(), 1, 10), vec![Rect::new((10, 10).into(), (980, 580).into())]);
    }

    #[test]
    fn two_windows_split_side_by_side() {
        let r = dwindle(area(), 2, 10);
        assert_eq!(r[0], Rect::new((10, 10).into(), (485, 580).into()));
        assert_eq!(r[1], Rect::new((505, 10).into(), (485, 580).into()));
    }

    #[test]
    fn three_windows_alternate_direction() {
        let r = dwindle(area(), 3, 10);
        // right half (485x580) is taller than wide -> split top/bottom
        assert_eq!(r[1].loc.x, r[2].loc.x);
        assert!(r[2].loc.y > r[1].loc.y);
    }

    #[test]
    fn no_overlap_and_inside_area() {
        for n in 1..12 {
            let r = dwindle(area(), n, 8);
            assert_eq!(r.len(), n);
            for (i, a) in r.iter().enumerate() {
                assert!(a.size.w > 0 && a.size.h > 0);
                assert!(a.loc.x >= 0 && a.loc.y >= 0);
                assert!(a.loc.x + a.size.w <= 1000 && a.loc.y + a.size.h <= 600);
                for b in &r[i + 1..] {
                    assert!(a.intersection(*b).is_none(), "{a:?} overlaps {b:?} (n={n})");
                }
            }
        }
    }

    #[test]
    fn float_rect_is_centered() {
        let f = default_float_rect(area());
        assert_eq!(f, Rect::new((200, 120).into(), (600, 360).into()));
    }
}
