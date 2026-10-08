//! The stream window's rules that do not need Win32 (S50 "share and go"):
//! the title call apps list it by, the clean feed's size and position, the
//! letterbox inside it, and whether the window must be hidden from screen
//! capture right now. `host` does the Win32 half; everything here is pure so
//! it is tested without a window.
//!
//! The capture rule is the heart of it. B9 made the window invisible to every
//! capture, always, because a PC that shares its own screen while showing a
//! received stream captures the stream inside itself, without end. That also
//! made the window impossible to pick in Discord, Zoom, Teams, Meet or OBS,
//! which is the one thing a receiving PC in a call wants to do with it. So
//! the guard is now conditional: hidden only while *this* PC is sharing an
//! area the window is on, visible to capture the rest of the time.

use crate::command::{CleanFeed, SourceTarget};

/// The window class every Relay stream window has, in every mode. Stable on
/// purpose: OBS remembers a Window Capture source by title, class and exe.
pub const CLASS_NAME: &str = "RelayReceiver";

/// Longest sender name put in a title, in characters.
const MAX_NAME_CHARS: usize = 48;

/// A rectangle in desktop (virtual-screen) physical pixels; right/bottom
/// exclusive, as Win32's `RECT`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Rect {
    pub fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { left: x, top: y, right: x + w, bottom: y + h }
    }

    pub fn width(&self) -> i32 {
        self.right - self.left
    }

    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }

    pub fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }

    /// Do the two share at least one pixel? Touching edges do not count.
    pub fn intersects(&self, o: &Rect) -> bool {
        !self.is_empty()
            && !o.is_empty()
            && self.left < o.right
            && o.left < self.right
            && self.top < o.bottom
            && o.top < self.bottom
    }
}

/// The stream window's title: what Discord's Go Live, Zoom's and Teams'
/// window lists, Chrome's "A window" picker and OBS Window Capture show.
/// "Relay — from JAKE" once a sender has paired. The name is the sender's
/// own, so control characters are dropped and a long one is cut short
/// rather than pushing the useful part of the title out of a picker row.
pub fn window_title(sender: Option<&str>) -> String {
    let cleaned: String = sender.unwrap_or("").chars().filter(|c| !c.is_control()).collect();
    let name = cleaned.trim();
    if name.is_empty() {
        return "Relay — receiving".to_string();
    }
    let name = if name.chars().count() > MAX_NAME_CHARS {
        let cut: String = name.chars().take(MAX_NAME_CHARS).collect();
        format!("{}…", cut.trim_end())
    } else {
        name.to_string()
    };
    format!("Relay — from {name}")
}

/// Where a clean feed goes: exactly the feed's size, centred in the work
/// area of the monitor the stream came from. A feed bigger than that area
/// (1440p on a 1080p monitor) keeps its size — the point of a clean feed is
/// that the size never changes — and starts at the area's top-left, so what
/// runs off screen is the bottom-right, never the corner you look at.
/// Capture apps take the whole window either way.
pub fn clean_feed_rect(work: Rect, feed: CleanFeed) -> Rect {
    let (w, h) = feed.size();
    let (w, h) = (w as i32, h as i32);
    let left = work.left + ((work.width() - w) / 2).max(0);
    let top = work.top + ((work.height() - h) / 2).max(0);
    Rect::new(left, top, w, h)
}

/// The rectangle a `src` picture fills inside a `dst` surface, keeping its
/// aspect and centred, in integer pixels: black bars rather than a stretched
/// or cropped picture. An unknown source fills the surface.
pub fn letterbox(dst: (u32, u32), src: (u32, u32)) -> Rect {
    let (dw, dh) = (dst.0 as i32, dst.1 as i32);
    if dw <= 0 || dh <= 0 {
        return Rect::default();
    }
    if src.0 == 0 || src.1 == 0 {
        return Rect::new(0, 0, dw, dh);
    }
    let scale = (dw as f64 / src.0 as f64).min(dh as f64 / src.1 as f64);
    let vw = ((src.0 as f64 * scale).round() as i32).clamp(1, dw);
    let vh = ((src.1 as f64 * scale).round() as i32).clamp(1, dh);
    Rect::new((dw - vw) / 2, (dh - vh) / 2, vw, vh)
}

/// What a local share is capturing, resolved to the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SharedArea {
    /// A monitor, or a region of one, in desktop coordinates.
    Rect(Rect),
    /// One window, by HWND. Window capture takes that window's own pixels
    /// only, so another window over it is never in the picture.
    Window(u64),
    /// Sharing, but the target could not be resolved (a monitor index that
    /// no longer exists). Treated as covering everything: the recursion is
    /// the failure that cannot be allowed.
    Unknown,
}

/// Resolve a share target against this PC's monitors, in the order
/// `d3d::monitors()` returns them (the order `SourceTarget` indexes refer to).
pub fn shared_area(target: &SourceTarget, monitors: &[Rect]) -> SharedArea {
    match *target {
        SourceTarget::Display { index } => {
            monitors.get(index).map_or(SharedArea::Unknown, |m| SharedArea::Rect(*m))
        }
        SourceTarget::Region { display, x, y, w, h } => match monitors.get(display) {
            Some(m) => {
                SharedArea::Rect(Rect::new(m.left + x as i32, m.top + y as i32, w as i32, h as i32))
            }
            None => SharedArea::Unknown,
        },
        SourceTarget::Window { hwnd } => SharedArea::Window(hwnd),
    }
}

/// Must the stream window be hidden from screen capture right now?
///
/// Only when this PC is sharing and the share would contain the window: a
/// shared display or region the window overlaps, or the window itself. Any
/// other time it stays capturable, so a call app on this PC can pick it.
pub fn should_exclude(local: Option<&SharedArea>, window: Rect, own_hwnd: u64) -> bool {
    match local {
        None => false,
        Some(SharedArea::Unknown) => true,
        Some(SharedArea::Rect(r)) => r.intersects(&window),
        Some(SharedArea::Window(h)) => *h == own_hwnd,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEFT: Rect = Rect { left: 0, top: 0, right: 2560, bottom: 1440 };
    const RIGHT: Rect = Rect { left: 2560, top: 0, right: 4480, bottom: 1080 };

    #[test]
    fn titles_name_the_sender() {
        assert_eq!(window_title(Some("JAKE")), "Relay — from JAKE");
        assert_eq!(window_title(Some("  den-pc  ")), "Relay — from den-pc");
        assert_eq!(window_title(None), "Relay — receiving");
        assert_eq!(window_title(Some("   ")), "Relay — receiving");
        assert_eq!(window_title(Some("a\u{7}b\nc")), "Relay — from abc", "no control chars");
        let long = "x".repeat(80);
        let t = window_title(Some(&long));
        assert_eq!(t, format!("Relay — from {}…", "x".repeat(MAX_NAME_CHARS)));
        // Characters, not bytes: a non-ASCII name is not cut mid-character.
        let wide = "é".repeat(60);
        assert!(window_title(Some(&wide)).ends_with("é…"));
    }

    #[test]
    fn the_class_name_is_stable() {
        assert_eq!(CLASS_NAME, "RelayReceiver");
    }

    #[test]
    fn clean_feed_is_always_exactly_its_size() {
        let work = Rect::new(0, 0, 2560, 1400);
        for feed in [CleanFeed::Fhd, CleanFeed::Qhd] {
            let r = clean_feed_rect(work, feed);
            assert_eq!((r.width() as u32, r.height() as u32), feed.size(), "{feed:?}");
        }
    }

    #[test]
    fn clean_feed_centres_in_the_work_area() {
        // 1080p feed on a 1440p monitor whose taskbar takes 40 px.
        let r = clean_feed_rect(Rect::new(0, 0, 2560, 1400), CleanFeed::Fhd);
        assert_eq!(r, Rect::new(320, 160, 1920, 1080));
        // On a second monitor to the right, origin offset.
        let r = clean_feed_rect(Rect::new(2560, 0, 1920, 1040), CleanFeed::Fhd);
        assert_eq!(r, Rect::new(2560, 0, 1920, 1080), "taller than the area: pinned to its top");
    }

    #[test]
    fn a_feed_bigger_than_the_monitor_keeps_its_size_from_the_top_left() {
        let work = Rect::new(-1920, 0, 1920, 1040);
        let r = clean_feed_rect(work, CleanFeed::Qhd);
        assert_eq!(r, Rect::new(-1920, 0, 2560, 1440));
    }

    #[test]
    fn letterbox_keeps_the_aspect() {
        assert_eq!(letterbox((1920, 1080), (1920, 1080)), Rect::new(0, 0, 1920, 1080));
        assert_eq!(letterbox((1920, 1080), (2560, 1440)), Rect::new(0, 0, 1920, 1080));
        assert_eq!(letterbox((2560, 1440), (1280, 720)), Rect::new(0, 0, 2560, 1440));
        // Ultrawide into 16:9: bars top and bottom.
        assert_eq!(letterbox((1920, 1080), (3440, 1440)), Rect::new(0, 138, 1920, 804));
        // 4:3 into 16:9: bars left and right.
        assert_eq!(letterbox((1920, 1080), (1440, 1080)), Rect::new(240, 0, 1440, 1080));
        assert_eq!(letterbox((1920, 1080), (0, 0)), Rect::new(0, 0, 1920, 1080));
        assert_eq!(letterbox((0, 1080), (1920, 1080)), Rect::default());
    }

    #[test]
    fn targets_resolve_against_the_monitor_list() {
        let mons = [LEFT, RIGHT];
        assert_eq!(
            shared_area(&SourceTarget::Display { index: 1 }, &mons),
            SharedArea::Rect(RIGHT)
        );
        assert_eq!(shared_area(&SourceTarget::Display { index: 2 }, &mons), SharedArea::Unknown);
        assert_eq!(
            shared_area(&SourceTarget::Region { display: 1, x: 10, y: 20, w: 300, h: 200 }, &mons),
            SharedArea::Rect(Rect::new(2570, 20, 300, 200))
        );
        assert_eq!(
            shared_area(&SourceTarget::Region { display: 5, x: 0, y: 0, w: 1, h: 1 }, &mons),
            SharedArea::Unknown
        );
        assert_eq!(shared_area(&SourceTarget::Window { hwnd: 77 }, &mons), SharedArea::Window(77));
    }

    #[test]
    fn capturable_unless_this_pc_shares_what_the_window_is_on() {
        let on_left = Rect::new(100, 100, 1280, 720);
        let on_right = Rect::new(2700, 100, 1280, 720);
        let left = SharedArea::Rect(LEFT);
        // Not sharing: always capturable, which is the point of S50.
        assert!(!should_exclude(None, on_left, 1));
        // Sharing the monitor the window is on: the B9 recursion. Hidden.
        assert!(should_exclude(Some(&left), on_left, 1));
        // Sharing the other monitor: nothing to recurse into. Capturable.
        assert!(!should_exclude(Some(&left), on_right, 1));
        // A window straddling both monitors is on the shared one too.
        assert!(should_exclude(Some(&left), Rect::new(2000, 0, 1000, 500), 1));
        // Exactly touching the shared monitor's edge is not on it.
        assert!(!should_exclude(Some(&left), Rect::new(2560, 0, 100, 100), 1));
        // A region share only covers its region.
        let region = SharedArea::Rect(Rect::new(0, 0, 50, 50));
        assert!(!should_exclude(Some(&region), on_left, 1));
        // Window share: only sharing the stream window itself recurses.
        assert!(!should_exclude(Some(&SharedArea::Window(9)), on_left, 1));
        assert!(should_exclude(Some(&SharedArea::Window(1)), on_left, 1));
        // Unknown: hidden, wherever the window is.
        assert!(should_exclude(Some(&SharedArea::Unknown), on_right, 1));
        // A zero-size window (created hidden, not placed yet) overlaps nothing.
        assert!(!should_exclude(Some(&left), Rect::default(), 1));
    }

    /// The decision is re-taken on every event that can change it; walk one
    /// session through them the way the window thread sees them.
    #[test]
    fn start_stop_and_move_transitions() {
        let mons = [LEFT, RIGHT];
        let mut local: Option<SharedArea> = None;
        let mut window = Rect::new(100, 100, 1280, 720);
        let decide = |l: &Option<SharedArea>, w: Rect| should_exclude(l.as_ref(), w, 1);
        assert!(!decide(&local, window), "receiving alone: capturable");
        local = Some(shared_area(&SourceTarget::Display { index: 0 }, &mons));
        assert!(decide(&local, window), "a local share of this monitor starts: hidden");
        window = Rect::new(2700, 100, 1280, 720);
        assert!(!decide(&local, window), "moved to the other monitor: capturable again");
        local = Some(shared_area(&SourceTarget::Display { index: 1 }, &mons));
        assert!(decide(&local, window), "the share switches to that monitor: hidden");
        local = None;
        assert!(!decide(&local, window), "the local share stops: capturable");
    }
}
