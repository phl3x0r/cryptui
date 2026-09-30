//! Colours and styles, in one place so the look can be adjusted without
//! touching layout code.

use ratatui::style::{Color, Modifier, Style};

/// Titles, borders and other structural accents.
pub(crate) const ACCENT: Color = Color::Cyan;
/// Secondary text such as headings and units.
pub(crate) const LABEL: Color = Color::DarkGray;
/// Profit.
pub(crate) const POSITIVE: Color = Color::Green;
/// Loss.
pub(crate) const NEGATIVE: Color = Color::Red;
/// Degraded but not failed.
pub(crate) const WARNING: Color = Color::Yellow;
/// Failure.
pub(crate) const ERROR: Color = Color::Red;
/// The reference line marking the newest close on the chart.
pub(crate) const LAST_PRICE: Color = Color::DarkGray;

/// Style for a profit or loss value: green when positive, red when negative,
/// neutral at exactly zero.
pub(crate) fn pnl_style(value: f64) -> Style {
    if value > 0.0 {
        Style::default().fg(POSITIVE)
    } else if value < 0.0 {
        Style::default().fg(NEGATIVE)
    } else {
        Style::default().fg(LABEL)
    }
}

/// Style for the long/short marker.
pub(crate) fn side_style(side: crate::venue::PositionSide) -> Style {
    match side {
        crate::venue::PositionSide::Long => Style::default().fg(POSITIVE),
        crate::venue::PositionSide::Short => Style::default().fg(NEGATIVE),
    }
}

/// Style used for the selected table row.
pub(crate) fn selection_style() -> Style {
    Style::default()
        .add_modifier(Modifier::REVERSED)
        .add_modifier(Modifier::BOLD)
}

/// Border style for a panel that is currently focused.
pub(crate) fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(ACCENT)
    } else {
        Style::default().fg(LABEL)
    }
}
