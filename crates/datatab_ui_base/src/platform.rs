/// Platform detection utilities for window management.
///
/// Different window systems have different behaviors and requirements.
/// This module provides helpers to detect the current platform and
/// adjust window creation accordingly.
use datatab_components::icons::AppIcon;
#[cfg(any(target_os = "linux", target_os = "windows"))]
use datatab_components::primitives::Icon;
use datatab_components::primitives::Text;
#[cfg(target_os = "linux")]
use datatab_components::tokens::ChromeColors;
use datatab_components::tokens::ShellMetrics;
#[cfg(target_os = "linux")]
use datatab_components::tokens::{IslandMetrics, Spacing};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use gpui::ClickEvent;
#[cfg(target_os = "linux")]
use gpui::Decorations;
use gpui::InteractiveElement;
use gpui::{
    App, IntoElement, SharedString, Stateful, Window, WindowDecorations, WindowKind, WindowOptions,
    div, px,
};
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos"))]
use gpui::{ParentElement, StatefulInteractiveElement, Styled};
#[cfg(any(target_os = "linux", target_os = "windows"))]
use gpui_component::ActiveTheme;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use gpui_component::InteractiveElementExt;

/// A single breadcrumb entry for the CSD title bar.
pub struct TitleCrumb {
    pub icon: Option<AppIcon>,
    pub label: SharedString,
}

/// Action run by a CSD title-bar button.
///
/// Passed as `on_close` to replace the close button's default of removing the
/// window directly: a window whose close must go through its own checks (the
/// main window's quit prompt and graceful shutdown) passes the handler that
/// runs them.
pub type TitleBarHandler = Box<dyn Fn(&mut Window, &mut App) + 'static>;

/// Title bar height for Linux CSD mode. Used for layout and client inset reporting.
pub const TITLE_BAR_HEIGHT: gpui::Pixels = px(32.0);

/// Request client decorations on every Linux desktop.
#[cfg(target_os = "linux")]
pub fn decoration_request() -> Option<WindowDecorations> {
    Some(WindowDecorations::Client)
}

/// Returns the `WindowDecorations` value to request when creating a top-level window.
///
/// On non-Linux platforms, returns `Server` explicitly to preserve original behavior
/// (not `None`, which leaves the decision to the platform default and could differ).
#[cfg(not(target_os = "linux"))]
pub fn decoration_request() -> Option<WindowDecorations> {
    Some(WindowDecorations::Server)
}

/// Backward-compatible alias used by main window creation in `main.rs`.
pub use decoration_request as main_window_decoration_request;

/// Returns `true` if the window is in client-side decoration (CSD) mode.
///
/// On Linux, checks if `window.window_decorations()` returns `Decorations::Client`.
/// On other platforms, always returns `false`.
#[cfg(target_os = "linux")]
pub fn should_render_csd(window: &Window) -> bool {
    matches!(window.window_decorations(), Decorations::Client { .. })
}

/// Returns `false` on non-Linux platforms (no CSD support needed).
#[cfg(not(target_os = "linux"))]
pub fn should_render_csd(_window: &Window) -> bool {
    false
}

/// Conditionally renders a CSD title bar for Linux and configures the client inset.
///
/// Call this at the start of every top-level window's `Render::render()` and store
/// the result. Prepend it as the first child of the root flex column.
///
/// Returns `Some(element)` when CSD is active (Linux Wayland with compositor granting
/// CSD), `None` otherwise. When `None` is returned on Linux, the client inset is
/// explicitly reset to zero to prevent stale insets.
///
/// Pass `crumbs` to render a breadcrumb trail after the app name. An empty slice
/// renders the title alone (same behavior as before).
pub fn render_csd_title_bar(
    window: &mut Window,
    cx: &mut App,
    title: &str,
) -> Option<Stateful<gpui::Div>> {
    render_csd_title_bar_with_crumbs(window, cx, title, &[], None)
}

/// Like [`render_csd_title_bar`] but accepts an optional breadcrumb trail displayed
/// after the app name: `DataTab  ›  {crumb1}  ›  {crumb2}`.
///
/// `on_close` replaces the close button's action; `None` keeps the default of
/// removing the window.
pub fn render_csd_title_bar_with_crumbs(
    window: &mut Window,
    cx: &mut App,
    title: &str,
    crumbs: &[TitleCrumb],
    on_close: Option<TitleBarHandler>,
) -> Option<Stateful<gpui::Div>> {
    if !prepare_client_decorations(window) {
        return None;
    }

    // Only the Linux CSD branch reads these; the signature stays uniform so
    // callers do not need their own cfg.
    #[cfg(not(target_os = "linux"))]
    let _ = (cx, title, crumbs, on_close);

    #[cfg(target_os = "linux")]
    {
        let theme = cx.theme();
        let sep_color = ChromeColors::ghost_border(theme);
        // The row sits on the window's desk frame: no fill and no line.
        let title_bar = div()
            .id("linux-csd-title-bar")
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .h(IslandMetrics::WINDOW_TITLE_HEIGHT);

        let mut drag_area = csd_drag_area("linux-csd-drag-area", window, cx)
            .flex()
            .flex_row()
            .items_center()
            .flex_1()
            .h_full()
            .pl(IslandMetrics::WINDOW_TITLE_PADDING_X)
            .gap_2()
            .child(Text::body_sm(title.to_string()));

        for crumb in crumbs {
            drag_area = drag_area
                .child(
                    div()
                        .w(px(1.0))
                        .h(Spacing::MD)
                        .bg(sep_color)
                        .flex_shrink_0(),
                )
                .child({
                    let mut crumb_el = div().flex().flex_row().items_center().gap(Spacing::XS);

                    if let Some(icon) = crumb.icon {
                        crumb_el = crumb_el.child(Icon::new(icon).size(Spacing::MD).muted());
                    }

                    crumb_el.child(Text::body_sm(crumb.label.clone()))
                });
        }

        Some(
            title_bar
                .child(drag_area)
                .child(render_csd_window_controls(window, cx, on_close)),
        )
    }

    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// The title row for Settings and Connection Manager.
pub fn render_shell_title_bar(
    window: &mut Window,
    cx: &mut App,
    title: &str,
) -> Option<Stateful<gpui::Div>> {
    let client_decorated = prepare_client_decorations(window);
    if cfg!(target_os = "linux") && !client_decorated {
        return None;
    }
    let draw_controls =
        (client_decorated || cfg!(target_os = "windows")) && !window.is_fullscreen();
    let controls = draw_controls.then(|| render_csd_window_controls(window, cx, None));
    let reserve = if cfg!(target_os = "macos") && !window.is_fullscreen() {
        ShellMetrics::MAC_TRAFFIC_LIGHT_RESERVE
    } else {
        px(0.0)
    };
    Some(
        div()
            .id("shell-window-title-bar")
            .debug_selector(|| "shell-window-title-bar".to_string())
            .flex()
            .flex_shrink_0()
            .items_center()
            .h(ShellMetrics::TITLE_BAR_HEIGHT)
            .pl(reserve)
            .child(
                csd_drag_area("shell-window-title-drag", window, cx)
                    .flex()
                    .items_center()
                    .flex_1()
                    .h_full()
                    .pl(if reserve == px(0.0) {
                        ShellMetrics::TITLE_BAR_PADDING_X
                    } else {
                        px(0.0)
                    })
                    .child(Text::body_sm(title.to_string())),
            )
            .children(controls),
    )
}

/// Configure the caption on windows that render a shell title row.
pub fn apply_shell_titlebar_options(options: &mut WindowOptions) {
    if let Some(titlebar) = options.titlebar.as_mut() {
        titlebar.appears_transparent = cfg!(any(target_os = "windows", target_os = "macos"));
        if cfg!(target_os = "macos") {
            titlebar.traffic_light_position = Some(gpui::point(
                ShellMetrics::MAC_TRAFFIC_LIGHT_POSITION,
                ShellMetrics::MAC_TRAFFIC_LIGHT_POSITION,
            ));
        }
    }
    options.app_owns_titlebar_drag = cfg!(target_os = "macos");
}

/// Reports whether the app draws its own title bar this frame and sets the
/// client inset to match.
///
/// Call once per render of a top-level window that embeds its own title bar
/// row. When CSD is active the inset is set to `TITLE_BAR_HEIGHT`; otherwise,
/// on Linux, it is reset to zero so a window that left CSD mode does not keep
/// a stale inset.
pub fn prepare_client_decorations(window: &mut Window) -> bool {
    if !should_render_csd(window) {
        #[cfg(target_os = "linux")]
        window.set_client_inset(px(0.0));
        return false;
    }

    window.set_client_inset(TITLE_BAR_HEIGHT);
    true
}

/// The window-management area of a title bar. It delegates dragging and
/// double-click behavior to the platform.
pub fn csd_drag_area(
    id: impl Into<gpui::ElementId>,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<gpui::Div> {
    let id = id.into();
    #[cfg(not(target_os = "macos"))]
    let (_window, _cx) = (window, cx);
    #[cfg(target_os = "macos")]
    let pressed = window.use_keyed_state(id.clone(), cx, |_, _| false);
    let area = div().id(id);

    #[cfg(target_os = "linux")]
    let area = area
        .cursor_pointer()
        .on_mouse_down(gpui::MouseButton::Left, |_, window, _cx| {
            window.start_window_move();
        })
        .on_double_click(|_: &ClickEvent, window: &mut Window, _cx: &mut App| {
            window.zoom_window();
        })
        .on_mouse_down(
            gpui::MouseButton::Right,
            |event: &gpui::MouseDownEvent, window: &mut Window, _cx: &mut App| {
                window.show_window_menu(event.position);
            },
        );

    #[cfg(target_os = "macos")]
    let area = area
        .on_mouse_down_out(window.listener_for(&pressed, |pressed, _, _, _| *pressed = false))
        .on_mouse_down(
            gpui::MouseButton::Left,
            window.listener_for(&pressed, |pressed, _, _, _| *pressed = true),
        )
        .on_mouse_up(
            gpui::MouseButton::Left,
            window.listener_for(&pressed, |pressed, _, _, _| *pressed = false),
        )
        .on_mouse_move(window.listener_for(
            &pressed,
            |pressed, event: &gpui::MouseMoveEvent, window, _| {
                if event.pressed_button == Some(gpui::MouseButton::Left) && *pressed {
                    *pressed = false;
                    window.start_window_move();
                }
            },
        ))
        .on_double_click(|_: &ClickEvent, window: &mut Window, _cx: &mut App| {
            window.titlebar_double_click();
        });

    #[cfg(target_os = "windows")]
    let area = area.window_control_area(gpui::WindowControlArea::Drag);

    area
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
#[derive(Clone, Copy)]
enum WindowControl {
    Minimize,
    Maximize,
    Close,
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
impl WindowControl {
    fn icon(self) -> AppIcon {
        match self {
            Self::Minimize => AppIcon::Minus,
            Self::Maximize => AppIcon::WindowSquare,
            Self::Close => AppIcon::X,
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Minimize => "csd-minimize",
            Self::Maximize => "csd-maximize",
            Self::Close => "csd-close",
        }
    }

    fn label(self) -> String {
        match self {
            Self::Minimize => datatab_i18n::t!("workspace.title_bar.minimize"),
            Self::Maximize => datatab_i18n::t!("workspace.title_bar.maximize"),
            Self::Close => datatab_i18n::t!("workspace.title_bar.close"),
        }
    }
}

#[cfg(any(target_os = "linux", target_os = "windows"))]
fn window_control_button(control: WindowControl, hover: gpui::Hsla) -> Stateful<gpui::Div> {
    div()
        .id(control.id())
        .debug_selector(|| control.id().to_string())
        .aria_label(control.label())
        .flex()
        .items_center()
        .justify_center()
        .w(ShellMetrics::WINDOW_CONTROL_WIDTH)
        .h_full()
        .flex_shrink_0()
        .hover(move |style| style.bg(hover))
        .child(
            Icon::new(control.icon())
                .size(ShellMetrics::WINDOW_CONTROL_ICON)
                .muted(),
        )
}

/// Minimize, maximize and close buttons of a CSD title bar, as the compositor
/// allows them, each 46 px wide and filling the bar's height.
///
/// On Linux `on_close` replaces the close button's action; `None` removes
/// the window. On Windows the system handles the marked control areas.
pub fn render_csd_window_controls(
    window: &mut Window,
    cx: &mut App,
    on_close: Option<TitleBarHandler>,
) -> gpui::AnyElement {
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    let (_window, _cx, _on_close) = (window, cx, on_close);

    #[cfg(target_os = "windows")]
    {
        let (_window, _on_close) = (window, on_close);
        let button = |control: WindowControl, area, hover| {
            window_control_button(control, hover).window_control_area(area)
        };
        div()
            .flex()
            .h_full()
            .flex_shrink_0()
            .child(button(
                WindowControl::Minimize,
                gpui::WindowControlArea::Min,
                cx.theme().secondary_hover,
            ))
            .child(button(
                WindowControl::Maximize,
                gpui::WindowControlArea::Max,
                cx.theme().secondary_hover,
            ))
            .child(button(
                WindowControl::Close,
                gpui::WindowControlArea::Close,
                cx.theme().danger,
            ))
            .into_any_element()
    }

    #[cfg(target_os = "linux")]
    {
        let controls = window.window_controls();
        let hover = cx.theme().secondary_hover;
        let close_hover = cx.theme().danger;
        let make_button = |control, handler: TitleBarHandler, hover| {
            window_control_button(control, hover)
                .cursor_pointer()
                .on_click(move |_, window, cx| handler(window, cx))
        };
        let mut row = div()
            .flex()
            .flex_row()
            .flex_shrink_0()
            .items_center()
            .h_full();
        if controls.minimize {
            row = row.child(make_button(
                WindowControl::Minimize,
                Box::new(|window, _cx| window.minimize_window()),
                hover,
            ));
        }
        if controls.maximize {
            row = row.child(make_button(
                WindowControl::Maximize,
                Box::new(|window, _cx| window.zoom_window()),
                hover,
            ));
        }
        let close_handler =
            on_close.unwrap_or_else(|| Box::new(|window, _cx| window.remove_window()));
        row.child(make_button(
            WindowControl::Close,
            close_handler,
            close_hover,
        ))
        .into_any_element()
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        div().into_any_element()
    }
}

/// Backward-compatible alias: renders the title bar with a fixed "DataTab" title.
/// Prefer `render_csd_title_bar` for new code that needs per-window titles.
pub fn render_linux_title_bar(window: &mut Window, cx: &mut App) -> impl IntoElement + 'static {
    match render_csd_title_bar(window, cx, "DataTab") {
        Some(el) => el.into_any_element(),
        None => div().into_any_element(),
    }
}

/// Returns true if running on X11 (not Wayland, macOS, or Windows).
///
/// Detection is based on environment variables:
/// - `WAYLAND_DISPLAY` indicates Wayland
/// - `DISPLAY` indicates X11 (if WAYLAND_DISPLAY is not set)
pub fn is_x11() -> bool {
    #[cfg(target_os = "linux")]
    {
        // If WAYLAND_DISPLAY is set, we're on Wayland, not X11
        if std::env::var("WAYLAND_DISPLAY").is_ok() {
            return false;
        }

        // If DISPLAY is set and WAYLAND_DISPLAY is not, we're on X11
        std::env::var("DISPLAY").is_ok()
    }

    #[cfg(not(target_os = "linux"))]
    {
        // macOS and Windows don't use X11
        false
    }
}

/// Returns the window kind for secondary windows (Settings, Connection Manager, etc.).
///
/// `WindowKind::Floating` on every platform. On Linux, gpui parents a floating
/// window to the window that holds keyboard focus when it opens: Wayland sets
/// the `xdg_toplevel` parent and X11 sets `WM_TRANSIENT_FOR`. Tiling
/// compositors such as Hyprland float a window with a parent at its requested
/// size instead of tiling it. The window is not modal: `WindowKind::Dialog`
/// would block input to its parent.
pub fn floating_window_kind() -> WindowKind {
    WindowKind::Floating
}

/// Space kept free between a secondary window and each edge of the display's
/// visible area when the requested size does not fit. (24 px)
pub(crate) const WINDOW_SCREEN_MARGIN: f32 = 24.0;

/// Shrinks `requested` so it fits inside `available` minus the screen margin
/// on every side. A dimension that already fits is kept.
pub fn fit_window_size(
    requested: gpui::Size<gpui::Pixels>,
    available: gpui::Size<gpui::Pixels>,
) -> gpui::Size<gpui::Pixels> {
    let margin = px(WINDOW_SCREEN_MARGIN * 2.0);
    let max_width = (available.width - margin).max(px(0.0));
    let max_height = (available.height - margin).max(px(0.0));

    gpui::Size {
        width: requested.width.min(max_width),
        height: requested.height.min(max_height),
    }
}

/// Shrinks `area` by [`WINDOW_SCREEN_MARGIN`] on every side: the region a
/// window should stay inside for its frame to remain on screen. A dimension
/// smaller than twice the margin collapses to zero rather than inverting.
pub fn inset_by_screen_margin(area: gpui::Bounds<gpui::Pixels>) -> gpui::Bounds<gpui::Pixels> {
    let margin = px(WINDOW_SCREEN_MARGIN * 2.0);
    let width = (area.size.width - margin).max(px(0.0));
    let height = (area.size.height - margin).max(px(0.0));

    gpui::Bounds::new(
        gpui::point(
            area.origin.x + px(WINDOW_SCREEN_MARGIN),
            area.origin.y + px(WINDOW_SCREEN_MARGIN),
        ),
        gpui::size(width, height),
    )
}

/// Bounds for a new secondary window of `width` by `height`, shrunk to fit
/// the primary display's visible area (see [`fit_window_size`]) and centered
/// in it. Without a known display the requested size is centered as-is.
pub fn fitted_window_bounds(width: f32, height: f32, cx: &App) -> gpui::Bounds<gpui::Pixels> {
    let requested = gpui::size(px(width), px(height));

    match cx.primary_display() {
        Some(display) => {
            let visible = display.visible_bounds();
            let fitted = fit_window_size(requested, visible.size);

            gpui::Bounds::centered_at(visible.center(), fitted)
        }
        None => gpui::Bounds::centered(None, requested, cx),
    }
}

/// Applies DataTab window options to the main window: `WindowKind::Normal`, min
/// size so X11 window managers emit `WM_NORMAL_HINTS`, and platform-appropriate
/// decorations.
///
/// `Normal` is what makes the window the operating system's and every
/// accessibility-based window manager's to manage. `WindowKind::Floating`
/// opens an `NSPanel` above the other windows on macOS (see [`apply_window_options`]),
/// which takes the window out of AeroSpace, Spaces and Stage Manager.
pub fn apply_main_window_options(options: &mut WindowOptions, min_width: f32, min_height: f32) {
    apply_window_size_and_decorations(options, min_width, min_height);
    options.kind = WindowKind::Normal;
}

/// Applies standard DataTab window options for secondary windows (Settings, Connection
/// Manager, SSO Wizard, etc.): floating kind, min size so X11 window
/// managers emit `WM_NORMAL_HINTS`, and platform-appropriate decorations.
///
/// On Linux, requests CSD so secondary windows match the main window behavior and
/// render their own title bars. On other platforms, requests server-side decorations.
pub fn apply_window_options(options: &mut WindowOptions, min_width: f32, min_height: f32) {
    apply_window_size_and_decorations(options, min_width, min_height);
    options.kind = floating_window_kind();
}

fn apply_window_size_and_decorations(options: &mut WindowOptions, min_width: f32, min_height: f32) {
    // A minimum larger than the window it applies to would force the window
    // past the display, so it never exceeds the fitted initial size. A window
    // opened maximized or fullscreen still carries the size it was placed with.
    let initial_size = match options.window_bounds {
        Some(gpui::WindowBounds::Windowed(bounds))
        | Some(gpui::WindowBounds::Maximized(bounds))
        | Some(gpui::WindowBounds::Fullscreen(bounds)) => Some(bounds.size),
        None => None,
    };

    options.window_min_size = Some(gpui::Size {
        width: initial_size.map_or(px(min_width), |size| px(min_width).min(size.width)),
        height: initial_size.map_or(px(min_height), |size| px(min_height).min(size.height)),
    });

    options.window_decorations = decoration_request();
}

#[cfg(test)]
mod window_size_tests {
    use super::{
        WINDOW_SCREEN_MARGIN, apply_main_window_options, apply_window_options, fit_window_size,
        inset_by_screen_margin,
    };
    use gpui::{Bounds, WindowKind, WindowOptions, point, px, size};

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_requests_client_decorations() {
        assert_eq!(
            super::decoration_request(),
            Some(gpui::WindowDecorations::Client)
        );
    }

    #[test]
    fn the_screen_margin_is_inset_on_every_side() {
        let area = Bounds::new(point(px(0.0), px(0.0)), size(px(1920.0), px(1080.0)));
        let margin = px(WINDOW_SCREEN_MARGIN);

        let inset = inset_by_screen_margin(area);

        assert_eq!(inset.left(), margin);
        assert_eq!(inset.top(), margin);
        assert_eq!(inset.right(), area.right() - margin);
        assert_eq!(inset.bottom(), area.bottom() - margin);
    }

    #[test]
    fn a_work_area_narrower_than_the_margins_collapses_to_nothing() {
        let area = Bounds::new(point(px(100.0), px(100.0)), size(px(10.0), px(30.0)));
        let margin = px(WINDOW_SCREEN_MARGIN);

        let inset = inset_by_screen_margin(area);

        assert_eq!(inset.size, size(px(0.0), px(0.0)));
        assert_eq!(
            inset.origin,
            point(area.origin.x + margin, area.origin.y + margin)
        );
    }

    #[test]
    fn a_size_that_fits_the_display_is_kept() {
        let fitted = fit_window_size(size(px(1320.0), px(900.0)), size(px(2560.0), px(1400.0)));

        assert_eq!(fitted, size(px(1320.0), px(900.0)));
    }

    #[test]
    fn a_size_larger_than_the_display_shrinks_to_it_minus_the_margin() {
        let fitted = fit_window_size(size(px(1320.0), px(900.0)), size(px(1280.0), px(720.0)));

        assert_eq!(fitted, size(px(1232.0), px(672.0)));
    }

    #[test]
    fn each_dimension_is_fitted_on_its_own() {
        let fitted = fit_window_size(size(px(1180.0), px(1000.0)), size(px(1920.0), px(1000.0)));

        assert_eq!(fitted, size(px(1180.0), px(952.0)));
    }

    #[test]
    fn the_main_window_is_a_normal_window() {
        let mut options = WindowOptions::default();

        apply_main_window_options(&mut options, 800.0, 600.0);

        assert_eq!(options.kind, WindowKind::Normal);
    }

    #[test]
    fn a_secondary_window_floats() {
        let mut options = WindowOptions::default();

        apply_window_options(&mut options, 800.0, 600.0);

        assert_eq!(options.kind, WindowKind::Floating);
    }

    #[test]
    fn both_window_kinds_carry_the_min_size_and_the_decorations() {
        for (name, apply) in [
            (
                "main",
                apply_main_window_options as fn(&mut WindowOptions, f32, f32),
            ),
            ("secondary", apply_window_options),
        ] {
            let mut options = WindowOptions::default();

            apply(&mut options, 800.0, 600.0);

            assert_eq!(
                options.window_min_size,
                Some(size(px(800.0), px(600.0))),
                "{name} window"
            );
            assert!(options.window_decorations.is_some(), "{name} window");
        }
    }

    /// The min size never exceeds the window it applies to, whichever kind of
    /// window asks for it: a window placed maximized on a small display keeps
    /// the size it was placed with.
    #[test]
    fn a_min_size_larger_than_the_placed_window_shrinks_to_it() {
        let placed = size(px(600.0), px(400.0));

        for (name, apply) in [
            (
                "main",
                apply_main_window_options as fn(&mut WindowOptions, f32, f32),
            ),
            ("secondary", apply_window_options),
        ] {
            let mut options = WindowOptions {
                window_bounds: Some(gpui::WindowBounds::Windowed(gpui::Bounds::new(
                    point(px(0.0), px(0.0)),
                    placed,
                ))),
                ..WindowOptions::default()
            };

            apply(&mut options, 800.0, 600.0);

            assert_eq!(options.window_min_size, Some(placed), "{name} window");
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests;
