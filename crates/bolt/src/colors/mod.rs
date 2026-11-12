//! The neutral and semantic color roles of the installed palette.
//!
//! Screens read neutrals through [`Colors::for_current`] (or the crate-root
//! [`crate::colors`]) instead of a `gpui_component::Theme` field, the same
//! way they read accent roles through [`crate::brand`]. Each role holds the
//! value [`crate::theme::apply_theme`] installs for it, so reading a role and
//! reading the theme field it maps to draw the same color.

use gpui::{App, Hsla, rgb};

use crate::{
    palette::{self, PaletteMode},
    theme::{rgb_to_hsla, rgb_to_hsla_alpha},
};

/// The neutral, wash and semantic color roles of one palette.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Colors {
    /// The window ground the islands sit on.
    pub desk: Hsla,
    /// Inset fills below the panel level: title bar, sidebar, tab bar, editor.
    pub background: Hsla,
    /// Island, popover and table fill.
    pub panel: Hsla,
    /// Raised controls and secondary fills.
    pub raised: Hsla,
    /// Default border.
    pub line: Hsla,
    /// Stronger border for non-field surfaces.
    pub line_2: Hsla,
    /// Accessible edge of an input control.
    pub field_edge: Hsla,
    /// Divider between data rows.
    pub row_divider: Hsla,
    /// Titles, selected items and data values.
    pub strong: Hsla,
    /// Body text.
    pub foreground: Hsla,
    /// Metadata, labels, counts and descriptions.
    pub muted: Hsla,
    /// Hover wash over any surface.
    pub hover: Hsla,
    /// Wash of every other data row.
    pub alternating_row: Hsla,
    /// The hairline along an island outline.
    pub island_edge: Hsla,
    /// The dimmed desk behind a modal.
    pub scrim: Hsla,
    /// Success state.
    pub success: Hsla,
    /// Informational state and links.
    pub info: Hsla,
    /// Warning state.
    pub warning: Hsla,
    /// Errors and destructive actions.
    pub danger: Hsla,
    /// Text drawn on a solid success, info, warning or danger fill.
    pub on_semantic: Hsla,
}

impl Colors {
    /// The roles of the dark palette.
    pub fn dark() -> Self {
        Self {
            desk: rgb(0x040708).into(),
            background: rgb_to_hsla(0x080C0D),
            panel: rgb_to_hsla(0x0E1314),
            raised: rgb_to_hsla(0x171E1F),
            line: rgb_to_hsla(0x1F2829),
            line_2: rgb_to_hsla(0x303C3E),
            field_edge: rgb_to_hsla(0x566B6F),
            row_divider: rgb_to_hsla(0x141A1B),
            strong: rgb_to_hsla(0xF2F7F6),
            foreground: rgb_to_hsla(0xBFC9C8),
            muted: rgb_to_hsla(0x849190),
            hover: rgb_to_hsla_alpha(0xFFFFFF, 0.04),
            alternating_row: rgb_to_hsla_alpha(0xFFFFFF, 0.012),
            island_edge: Hsla::from(rgb(0xFFFFFF)).opacity(0.05),
            scrim: rgb_to_hsla_alpha(0x040708, 0.62),
            success: rgb_to_hsla(0x7BE0A0),
            info: rgb_to_hsla(0x6EA8FF),
            warning: rgb_to_hsla(0xFFC23D),
            danger: rgb_to_hsla(0xFF6B5E),
            on_semantic: rgb_to_hsla(0x080C0D),
        }
    }

    /// The roles of the light palette.
    pub fn light() -> Self {
        Self {
            desk: rgb(0xDCE5E4).into(),
            background: rgb_to_hsla(0xF2F6F5),
            panel: rgb_to_hsla(0xFFFFFF),
            raised: rgb_to_hsla(0xE7EEED),
            line: rgb_to_hsla(0xDAE3E2),
            line_2: rgb_to_hsla(0xBECBCA),
            field_edge: rgb_to_hsla(0x6C8785),
            row_divider: rgb_to_hsla(0xEBF0EF),
            strong: rgb_to_hsla(0x0E1716),
            foreground: rgb_to_hsla(0x303D3C),
            muted: rgb_to_hsla(0x5C6A69),
            hover: rgb_to_hsla_alpha(0x0E1716, 0.04),
            alternating_row: rgb_to_hsla_alpha(0x0E1716, 0.015),
            island_edge: Hsla::from(rgb(0x0E1716)).opacity(0.06),
            scrim: rgb_to_hsla_alpha(0x0E1F1E, 0.28),
            success: rgb_to_hsla(0x19703D),
            info: rgb_to_hsla(0x1E5CCA),
            warning: rgb_to_hsla(0x855809),
            danger: rgb_to_hsla(0xB23026),
            on_semantic: rgb_to_hsla(0xFFFFFF),
        }
    }

    /// The roles of the palette the product published; dark until it
    /// publishes one, and for an unresolved `System` request.
    pub fn for_current(cx: &App) -> Self {
        match palette::resolved_mode(cx) {
            PaletteMode::Light => Self::light(),
            PaletteMode::Dark | PaletteMode::System => Self::dark(),
        }
    }
}

#[cfg(test)]
mod tests;
