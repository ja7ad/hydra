// Copyright (C) 2026 Javad Rajabzadeh
// SPDX-License-Identifier: GPL-3.0-or-later

//! Download activity shown by icons of its own, apart from the Hydra logo:
//! a second tray icon that exists only while something is downloading, and
//! the icon and taskbar progress of each download's progress window.
//!
//! Values are reduced to a whole percent here, and to [`ICON_STEPS`] for the
//! drawn arrow, so the native surfaces are touched only when what they
//! display would change — never per arriving chunk.

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Indicator {
    #[default]
    Idle,
    /// Something is downloading and none of it has a known size.
    Indeterminate,
    Percent(u8),
}

/// How many fill levels the download arrow has: a panel draws it about
/// twenty pixels tall, so finer steps would redraw the icon for nothing.
pub const ICON_STEPS: u8 = 20;

/// The tray arrow is animated by a sheen that travels down it in this many
/// positions, one every [`SHEEN_INTERVAL_MS`]. The backends drive it from
/// native timers, so a step never becomes an iced message — and therefore
/// never a window repaint.
pub const SHEEN_PHASES: u8 = 8;
pub const SHEEN_INTERVAL_MS: u32 = 180;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IconFrame {
    #[default]
    Idle,
    Indeterminate,
    /// Filled `0..=ICON_STEPS` twentieths.
    Bar(u8),
}

impl Indicator {
    pub fn icon_frame(self) -> IconFrame {
        match self {
            Indicator::Idle => IconFrame::Idle,
            Indicator::Indeterminate => IconFrame::Indeterminate,
            Indicator::Percent(p) => {
                IconFrame::Bar((u16::from(p.min(100)) * u16::from(ICON_STEPS) / 100) as u8)
            }
        }
    }
}

/// Bytes done over bytes expected across the active downloads, given as
/// `(downloaded, size)`. Sizeless ones (live recordings, chunked responses)
/// only count towards "something is running".
pub fn aggregate(enabled: bool, active: impl IntoIterator<Item = (u64, Option<u64>)>) -> Indicator {
    if !enabled {
        return Indicator::Idle;
    }
    let (mut running, mut done, mut total) = (false, 0u128, 0u128);
    for (downloaded, size) in active {
        running = true;
        if let Some(size) = size.filter(|s| *s > 0) {
            total += u128::from(size);
            done += u128::from(downloaded.min(size));
        }
    }
    match (running, total) {
        (false, _) => Indicator::Idle,
        (true, 0) => Indicator::Indeterminate,
        (true, total) => Indicator::Percent((done * 100 / total) as u8),
    }
}

/// Show `next` on the download tray icon, which exists only while it is not
/// idle. `prev` is what it shows now, `None` when unknown. `animate` lets the
/// arrow move (off in power save).
pub fn show(prev: Option<Indicator>, next: Indicator, animate: bool) {
    let frame = next.icon_frame();
    if prev.map(Indicator::icon_frame) != Some(frame) {
        crate::tray::set_progress(frame, animate);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_active_is_idle() {
        assert_eq!(aggregate(true, []), Indicator::Idle);
    }

    #[test]
    fn active_downloads_sum_into_one_percentage() {
        let active = [(100, Some(400)), (0, Some(600))];
        assert_eq!(aggregate(true, active), Indicator::Percent(10));
    }

    #[test]
    fn unknown_sizes_are_indeterminate_until_one_is_known() {
        let (live, empty, sized) = ((4096, None), (7, Some(0)), (30, Some(60)));
        assert_eq!(aggregate(true, [live, empty]), Indicator::Indeterminate);
        assert_eq!(aggregate(true, [live, sized]), Indicator::Percent(50));
    }

    #[test]
    fn bytes_past_an_estimated_size_stop_at_full() {
        assert_eq!(aggregate(true, [(150, Some(100))]), Indicator::Percent(100));
        let huge = (u64::MAX, Some(u64::MAX));
        assert_eq!(aggregate(true, [huge, huge]), Indicator::Percent(100));
    }

    #[test]
    fn the_setting_switches_it_off() {
        assert_eq!(aggregate(false, [(1, Some(2))]), Indicator::Idle);
    }

    #[test]
    fn the_icon_fills_in_twentieths() {
        assert_eq!(Indicator::Idle.icon_frame(), IconFrame::Idle);
        assert_eq!(
            Indicator::Indeterminate.icon_frame(),
            IconFrame::Indeterminate
        );
        assert_eq!(Indicator::Percent(0).icon_frame(), IconFrame::Bar(0));
        assert_eq!(Indicator::Percent(4).icon_frame(), IconFrame::Bar(0));
        assert_eq!(Indicator::Percent(5).icon_frame(), IconFrame::Bar(1));
        assert_eq!(Indicator::Percent(99).icon_frame(), IconFrame::Bar(19));
        assert_eq!(Indicator::Percent(100).icon_frame(), IconFrame::Bar(20));
        assert_eq!(Indicator::Percent(250).icon_frame(), IconFrame::Bar(20));
    }

    #[test]
    fn showing_off_the_ui_thread_is_harmless() {
        show(None, Indicator::Percent(40), true);
        show(Some(Indicator::Percent(40)), Indicator::Percent(41), true);
        show(Some(Indicator::Percent(41)), Indicator::Idle, false);
    }
}
