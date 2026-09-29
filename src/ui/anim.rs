//! Pane transitions: a pane's contents glide the last few cells into place from the side they came
//! from, dimmed for the first half. Only cell positions and the terminal's own DIM attribute
//! change, so it looks right on any colour scheme (colour fades would need the terminal's
//! background colour, which isn't knowable).

use ratatui::{buffer::Buffer, layout::Rect, style::{Modifier, Style}};
use std::time::{Duration, Instant};

const DURATION: Duration = Duration::from_millis(180);
/// Cells the contents start away from their resting place (less in small panes).
const DISTANCE: u16 = 6;
/// Redraw interval while a transition runs.
pub const FRAME: Duration = Duration::from_millis(16);

/// Where the incoming contents start.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Side {
    Left,
    Right,
    Top,
    Bottom,
}

/// What the previous frame showed; a difference starts a transition.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Seen {
    /// Identity of the content view (kind, title, load id).
    pub view: u64,
    /// Back-stack depth: deeper means forward navigation.
    pub depth: usize,
    pub sidebar_focused: bool,
    pub fullscreen: bool,
}

#[derive(Default)]
pub struct Anim {
    seen: Option<Seen>,
    running: Option<(Rect, Side, Instant)>,
}

impl Anim {
    /// A transition is in progress, so frames must keep coming.
    pub fn running(&self) -> bool {
        self.running.is_some()
    }

    /// Records this frame's state, returning the previous one if it differs (never on the
    /// first frame, so startup doesn't animate).
    pub fn observe(&mut self, now: Seen) -> Option<Seen> {
        let before = self.seen.replace(now)?;
        (before != now).then_some(before)
    }

    pub fn start(&mut self, area: Rect, from: Side) {
        self.running = Some((area, from, Instant::now()));
    }

    /// Offsets and dims the transitioning area of a fully rendered frame.
    pub fn apply(&mut self, buf: &mut Buffer) {
        let Some((area, from, start)) = self.running else { return };
        let t = start.elapsed().as_secs_f32() / DURATION.as_secs_f32();
        let area = area.intersection(buf.area);
        if t >= 1.0 || area.is_empty() {
            self.running = None;
            return;
        }
        let ease_out = 1.0 - (1.0 - t).powi(3);
        let span = match from {
            Side::Left | Side::Right => area.width,
            Side::Top | Side::Bottom => area.height,
        };
        let offset = ((1.0 - ease_out) * f32::from(DISTANCE.min(span / 4))).round() as u16;
        shift(buf, area, from, offset);
        if t < 0.5 {
            buf.set_style(area, Style::new().add_modifier(Modifier::DIM));
        }
    }
}

/// Moves `area`'s cells `by` cells away from the `from` edge, blanking what they vacate.
fn shift(buf: &mut Buffer, area: Rect, from: Side, by: u16) {
    if by == 0 {
        return;
    }
    let (l, r, t, b) = (area.left(), area.right(), area.top(), area.bottom());
    let mut copy = |to: (u16, u16), src: Option<(u16, u16)>| match src {
        Some(src) => buf[to] = buf[src].clone(),
        None => {
            buf[to].reset();
        }
    };
    // Iterate against the direction of travel so every source is read before it's overwritten.
    match from {
        Side::Right => {
            for y in t..b {
                for x in (l..r).rev() {
                    copy((x, y), (x >= l + by).then(|| (x - by, y)));
                }
            }
        }
        Side::Left => {
            for y in t..b {
                for x in l..r {
                    copy((x, y), (x + by < r).then(|| (x + by, y)));
                }
            }
        }
        Side::Bottom => {
            for y in (t..b).rev() {
                for x in l..r {
                    copy((x, y), (y >= t + by).then(|| (x, y - by)));
                }
            }
        }
        Side::Top => {
            for y in t..b {
                for x in l..r {
                    copy((x, y), (y + by < b).then(|| (x, y + by)));
                }
            }
        }
    }
}
