use super::*;
use crate::{brand::Brand, density::Density, palette::PaletteMode, theme};
use gpui::{Context, Render, Rgba, TestAppContext, Window};

fn luminance(color: gpui::Hsla) -> f32 {
    let color = Rgba::from(color);
    let linear = |value: f32| {
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * linear(color.r) + 0.7152 * linear(color.g) + 0.0722 * linear(color.b)
}

#[gpui::test]
fn active_shape_joins_panel_and_tint_meets_contrast_in_both_modes(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let brand = Brand::neutral();
    for mode in [PaletteMode::Dark, PaletteMode::Light] {
        cx.update(|cx| theme::install(brand.clone(), mode, Density::Default, cx));
        cx.update(|cx| {
            let theme = cx.theme();
            let (fill, edge) = document_tab_appearance(true, theme);
            assert_eq!(fill, theme.popover, "active tab joins the panel");
            let edge = edge.expect("active tab has a top edge");
            assert_eq!(edge, ChromeColors::tint(theme));
            let (a, b) = (luminance(edge), luminance(fill));
            assert!(
                (a.max(b) + 0.05) / (a.min(b) + 0.05) >= 3.0,
                "{mode:?} indicator contrast"
            );
            let (fill, edge) = document_tab_appearance(false, theme);
            assert_eq!(fill, gpui::transparent_black());
            assert!(edge.is_none());
        });
    }
}

struct TabHost;

impl Render for TabHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        document_tab_bar(cx)
            .debug_selector(|| "bar".to_string())
            .w(TabMetrics::DOCUMENT_TAB_MAX_WIDTH)
            .child(document_tab("tab", true, cx))
    }
}

#[gpui::test]
fn document_tab_reaches_bottom_of_shorter_bar(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, window) = cx.add_window_view(|_, _| TabHost);
    window.run_until_parked();
    let bar = window.debug_bounds("bar").expect("bar laid out");
    let tab = window.debug_bounds("tab").expect("tab laid out");
    assert_eq!(tab.bottom(), bar.bottom(), "tab meets its content");
}
