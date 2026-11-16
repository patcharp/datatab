use gpui::{Context, IntoElement, Modifiers, Render, TestAppContext, Window, div, point, px};
use gpui_component::ActiveTheme;

struct WindowControls;

impl Render for WindowControls {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::{ParentElement, Styled};
        div()
            .h(px(44.0))
            .child(super::render_csd_window_controls(window, cx, None))
    }
}

#[gpui::test]
fn close_control_uses_danger_fill_on_hover(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);
    let (_, window) = cx.add_window_view(|_, _| WindowControls);
    window.simulate_mouse_move(point(px(115.0), px(22.0)), None, Modifiers::none());
    window.run_until_parked();
    let painted_danger = window.update(|window, cx| {
        let danger = cx.theme().danger;
        window
            .painted_quads()
            .iter()
            .any(|quad| quad.background.as_solid() == Some(danger))
    });
    assert!(
        painted_danger,
        "the hovered close control uses the danger fill"
    );
}

struct SecondaryTitle;

impl Render for SecondaryTitle {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        use gpui::ParentElement;
        div().children(super::render_shell_title_bar(window, cx, "Settings"))
    }
}

#[gpui::test]
#[cfg(target_os = "linux")]
fn secondary_title_row_is_hidden_with_server_decorations(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);
    let (_, window) = cx.add_window_view(|_, _| SecondaryTitle);
    window.run_until_parked();
    assert!(window.debug_bounds("shell-window-title-bar").is_none());
    let capture = crate::keyboard_coverage::FrameCapture::observe(window);
    let frame = capture.frame(window);
    assert!(
        frame
            .nodes()
            .all(|(_, node)| !node.id().starts_with("csd-"))
    );
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[test]
fn window_control_glyphs_match_the_design() {
    use super::WindowControl;
    assert_eq!(WindowControl::Minimize.icon().path(), "icons/ui/minus.svg");
    assert_eq!(
        WindowControl::Maximize.icon().path(),
        "icons/ui/window-square.svg"
    );
    assert_eq!(WindowControl::Close.icon().path(), "icons/ui/x.svg");
    assert!(include_str!("../../../../resources/icons/ui/minus.svg").contains("M5 12h14"));
    assert!(
        include_str!("../../../../resources/icons/ui/window-square.svg")
            .contains("<rect x=\"4\" y=\"4\" width=\"16\" height=\"16\"/>")
    );
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[gpui::test]
fn window_controls_are_46_by_44(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);
    let (_, window) = cx.add_window_view(|_, _| WindowControls);
    window.run_until_parked();
    for id in ["csd-minimize", "csd-maximize", "csd-close"] {
        let bounds = window.debug_bounds(id).expect("window control is rendered");
        assert_eq!(bounds.size, gpui::size(px(46.0), px(44.0)));
    }
}
