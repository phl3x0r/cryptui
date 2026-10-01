//! Chart maths: moving averages and the visible window.
//!
//! Deliberately free of terminal types, so the arithmetic that decides what a
//! candle chart shows can be tested directly.

use std::ops::Range;

use crate::venue::Kline;

/// Moving-average windows drawn on the price chart.
pub const MOVING_AVERAGE_WINDOWS: [usize; 3] = [7, 25, 99];

/// Simple moving average of the closing prices, one entry per candle.
///
/// Entries before enough candles exist are `None`, which the renderer uses to
/// start the line only where it is meaningful.
pub fn moving_average(candles: &[Kline], window: usize) -> Vec<Option<f64>> {
    let mut values = vec![None; candles.len()];
    if window == 0 || candles.len() < window {
        return values;
    }

    let mut sum: f64 = candles[..window].iter().map(|candle| candle.close).sum();
    values[window - 1] = Some(sum / window as f64);

    for index in window..candles.len() {
        sum += candles[index].close - candles[index - window].close;
        values[index] = Some(sum / window as f64);
    }

    values
}

/// Which candles are on screen.
///
/// The window is expressed as an absolute start index plus a width, so it stays
/// meaningful while candles stream in at the end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    start: usize,
    visible: usize,
}

impl Viewport {
    /// Fewest candles the chart will zoom in to.
    pub const MIN_VISIBLE: usize = 20;
    /// Most candles the chart will zoom out to.
    pub const MAX_VISIBLE: usize = 240;
    /// Width the chart opens with.
    pub const DEFAULT_VISIBLE: usize = 80;
    /// Candles a single pan step moves.
    pub const PAN_STEP: usize = 5;

    /// A viewport showing `visible` candles, anchored at the newest candle.
    pub fn new(visible: usize) -> Self {
        Self {
            start: 0,
            visible: visible.clamp(Self::MIN_VISIBLE, Self::MAX_VISIBLE),
        }
    }

    /// Number of candles the window holds.
    pub fn visible(&self) -> usize {
        self.visible
    }

    /// Index of the first visible candle.
    pub fn start(&self) -> usize {
        self.start
    }

    /// The slice of `len` candles currently on screen, `width` of them wide.
    ///
    /// The width is the caller's to decide rather than this type's: the chart
    /// draws candles on a grid of whole cells, so how many of them fit depends on
    /// the pane, not on what was asked for.
    pub fn range(&self, len: usize, width: usize) -> Range<usize> {
        if len == 0 {
            return 0..0;
        }
        let width = width.clamp(1, len);
        let max_start = len - width;
        let start = self.start.min(max_start);
        start..start + width
    }

    /// Whether the window is pinned to the newest candle.
    pub fn is_at_end(&self, len: usize, width: usize) -> bool {
        let range = self.range(len, width);
        range.end == len
    }

    /// Move the window by `delta` candles, stopping at both ends.
    ///
    /// Returns whether the window actually moved.
    pub fn pan(&mut self, delta: isize, len: usize, width: usize) -> bool {
        if len == 0 {
            return false;
        }
        let width = width.clamp(1, len);
        let max_start = (len - width) as isize;
        let current = self.start.min(max_start.max(0) as usize) as isize;
        let next = (current + delta).clamp(0, max_start.max(0));
        let moved = next != current;
        self.start = next as usize;
        moved
    }

    /// Widen or narrow the window, keeping the left edge where it is.
    ///
    /// A non-finite or non-positive factor is ignored: `NaN.clamp(..)` is `NaN`,
    /// and casting that to a width would silently empty the chart.
    pub fn zoom(&mut self, factor: f64) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        let scaled = (self.visible as f64 * factor).round();
        let next = scaled.clamp(Self::MIN_VISIBLE as f64, Self::MAX_VISIBLE as f64) as usize;
        self.visible = next;
    }

    /// Pin the window to the newest candle.
    pub fn pin_to_end(&mut self, len: usize, width: usize) {
        let width = width.clamp(1, len.max(1));
        self.start = len.saturating_sub(width);
    }
}

/// How a row of candles is laid out across the columns a pane has.
///
/// A Braille cell holds two dots and one **colour**: two candles in one cell
/// erase each other, and a body that is not a whole number of cells comes out an
/// uneven width. Every candle therefore gets the same whole number of cells, and
/// the same whole number of cells before the next one — which is why the chart
/// shows a few fewer candles than the viewport asked for: an even pitch is worth
/// more than the last of the count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandleLayout {
    /// Cells from one candle's first cell to the next one's.
    pub pitch: u16,
    /// Cells in a body.
    pub body: u16,
    /// Candles that fit at this pitch.
    pub candles: usize,
    /// Blank cells kept at the oldest end, less than one pitch.
    pub margin: u16,
}

impl CandleLayout {
    /// Fit `requested` candles into `columns` cells.
    ///
    /// `columns` of `None` means the pane has not said how wide it is: one cell
    /// per candle, and nothing to fit.
    pub fn fit(columns: Option<u16>, requested: usize) -> Self {
        let requested = requested.max(1);
        let Some(columns) = columns.map(u32::from).filter(|columns| *columns > 0) else {
            return Self {
                pitch: 1,
                body: 1,
                candles: requested,
                margin: 0,
            };
        };

        // The pitch closest to what was asked for: an even one is worth a few
        // candles either way, and rounding keeps the count as near the request as
        // the grid allows.
        let requested = requested as u32;
        let pitch = ((columns + requested / 2) / requested).max(1);
        let candles = (columns / pitch).max(1) as usize;
        // Bodies touch only when there is no room for daylight.
        let gap = if pitch > 1 { (pitch / 4).max(1) } else { 0 };

        Self {
            pitch: pitch as u16,
            body: (pitch - gap).max(1) as u16,
            candles,
            // Right-aligned, and the gap after the newest candle folded into the
            // margin, so the live candle sits against the price axis.
            margin: (columns - candles as u32 * pitch + gap) as u16,
        }
    }

    /// The cells a candle's body occupies.
    pub fn cells(&self, index: usize) -> Range<u16> {
        let left = self.margin + index as u16 * self.pitch;
        left..left + self.body
    }
}

#[cfg(test)]
mod tests {
    use crate::venue::Kline;

    use super::{CandleLayout, Viewport, moving_average};

    fn candle(close: f64) -> Kline {
        Kline {
            open_time_ms: 0,
            open: close,
            high: close,
            low: close,
            close,
            volume: 1.0,
            close_time_ms: 0,
            closed: true,
        }
    }

    fn candles(closes: &[f64]) -> Vec<Kline> {
        closes.iter().copied().map(candle).collect()
    }

    #[test]
    fn moving_average_has_no_value_until_the_window_is_full() {
        let series = candles(&[1.0, 2.0, 3.0, 4.0, 5.0]);
        let average = moving_average(&series, 3);

        assert_eq!(average.len(), 5);
        assert_eq!(average[..2], [None, None]);
        assert_eq!(average[2], Some(2.0), "(1+2+3)/3");
        assert_eq!(average[3], Some(3.0));
        assert_eq!(average[4], Some(4.0));
    }

    #[test]
    fn moving_average_of_a_longer_window_is_all_none() {
        let series = candles(&[1.0, 2.0]);
        assert_eq!(moving_average(&series, 25), vec![None, None]);
        assert_eq!(moving_average(&[], 7), Vec::<Option<f64>>::new());
    }

    #[test]
    fn moving_average_slides_instead_of_recomputing() {
        // A long series exercises the sliding path, not just the first window.
        let closes: Vec<f64> = (1..=200).map(f64::from).collect();
        let average = moving_average(&candles(&closes), 7);

        let expected_last: f64 = (194..=200).map(f64::from).sum::<f64>() / 7.0;
        assert_eq!(average[199], Some(expected_last));
        assert_eq!(average[6], Some(4.0), "first complete window is 1..=7");
    }

    #[test]
    fn a_window_wider_than_the_data_shows_everything() {
        let viewport = Viewport::new(Viewport::DEFAULT_VISIBLE);
        assert_eq!(viewport.range(10, viewport.visible()), 0..10);
        assert_eq!(viewport.range(0, viewport.visible()), 0..0);
        assert!(viewport.is_at_end(10, viewport.visible()));
    }

    #[test]
    fn panning_stops_at_both_ends() {
        let mut viewport = Viewport::new(20);
        let len = 100;

        viewport.pin_to_end(len, viewport.visible());
        assert_eq!(viewport.start(), 80);
        assert!(
            !viewport.pan(5, len, viewport.visible()),
            "already at the end"
        );
        assert_eq!(viewport.start(), 80, "the window never runs past the data");

        assert!(
            viewport.pan(-5, len, viewport.visible()),
            "panning into history moves"
        );
        assert_eq!(viewport.start(), 75);
        assert!(!viewport.is_at_end(len, viewport.visible()));
        assert_eq!(viewport.range(len, viewport.visible()), 75..95);

        assert!(viewport.pan(-75, len, viewport.visible()));
        assert_eq!(viewport.start(), 0, "clamped at the oldest candle");
        assert!(
            !viewport.pan(-5, len, viewport.visible()),
            "cannot pan before the first candle"
        );
    }

    #[test]
    fn zooming_stays_within_limits() {
        let mut viewport = Viewport::new(Viewport::DEFAULT_VISIBLE);

        for _ in 0..40 {
            viewport.zoom(0.8);
        }
        assert_eq!(viewport.visible(), Viewport::MIN_VISIBLE);

        for _ in 0..40 {
            viewport.zoom(1.25);
        }
        assert_eq!(viewport.visible(), Viewport::MAX_VISIBLE);

        for factor in [f64::NAN, f64::INFINITY, 0.0, -1.0] {
            viewport.zoom(factor);
            assert_eq!(
                viewport.visible(),
                Viewport::MAX_VISIBLE,
                "a factor of {factor} must not corrupt the window"
            );
        }
    }

    #[test]
    fn pinning_to_the_end_follows_new_candles() {
        let mut viewport = Viewport::new(50);
        viewport.pin_to_end(200, viewport.visible());
        assert_eq!(viewport.range(200, viewport.visible()), 150..200);

        // One more candle arrives: following keeps the window at the end.
        viewport.pin_to_end(201, viewport.visible());
        assert_eq!(viewport.range(201, viewport.visible()), 151..201);
        assert!(viewport.is_at_end(201, viewport.visible()));
    }

    #[test]
    fn candles_are_laid_out_on_an_even_pitch() {
        for columns in [12u16, 40, 95, 140, 195, 240] {
            for requested in [1usize, 2, 20, 33, 50, 80, 120, 240, 400] {
                let layout = CandleLayout::fit(Some(columns), requested);

                assert!(layout.pitch >= 1 && layout.body >= 1, "{layout:?}");
                assert!(
                    layout.body <= layout.pitch,
                    "a body fits a pitch: {layout:?}"
                );
                assert!(layout.candles >= 1, "{layout:?}");
                assert_eq!(layout.cells(0).start, layout.margin, "{layout:?}");

                // Every candle is the same number of cells, one pitch apart, and
                // the newest sits against the price axis.
                for index in 1..layout.candles {
                    let previous = layout.cells(index - 1);
                    let current = layout.cells(index);
                    assert_eq!(current.start - previous.start, layout.pitch, "{layout:?}");
                    assert_eq!(current.end - current.start, layout.body, "{layout:?}");
                }
                assert_eq!(
                    layout.cells(layout.candles - 1).end,
                    columns,
                    "{layout:?} leaves a ragged edge"
                );
            }
        }
    }

    #[test]
    fn bars_only_touch_when_there_is_no_room_for_daylight() {
        for columns in [12u16, 40, 95, 195] {
            for requested in [1usize, 5, 20, 50, 80, 120] {
                let layout = CandleLayout::fit(Some(columns), requested);
                if layout.pitch > 1 {
                    assert!(
                        layout.body < layout.pitch,
                        "{columns}x{requested}: bars touch: {layout:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn the_count_gives_way_to_an_even_pitch() {
        // An even grid is worth a few candles either way, but not many: the pitch
        // is the whole number nearest the one that would have fitted them all.
        for columns in [40u16, 95, 140, 195, 240, 400] {
            for requested in [20usize, 33, 40, 80, 120, 240] {
                let layout = CandleLayout::fit(Some(columns), requested);
                // The pane can always hold one candle per cell; past that the
                // count is the pane's, not the request's.
                let expected = f64::from(columns).min(requested as f64);
                let ratio = layout.candles as f64 / expected;
                assert!(
                    (0.5..1.6).contains(&ratio),
                    "{columns}x{requested}: {} candles is too far from the ask",
                    layout.candles
                );
            }
        }
    }

    #[test]
    fn an_unknown_pane_leaves_the_window_alone() {
        let layout = CandleLayout::fit(None, 80);
        assert_eq!(layout.candles, 80, "nothing to fit, nothing to give up");
        assert_eq!((layout.pitch, layout.body, layout.margin), (1, 1, 0));
        assert_eq!(
            CandleLayout::fit(Some(0), 80).candles,
            80,
            "a pane of no width says nothing either"
        );
    }

    #[test]
    fn the_window_shrinks_to_the_available_data() {
        let viewport = Viewport::new(80);
        assert_eq!(
            viewport.range(5, viewport.visible()),
            0..5,
            "no blank candles are invented"
        );
    }
}
