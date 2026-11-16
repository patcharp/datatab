use super::*;
use datatab_components::composites::{MenuItem, menu_frame, menu_row};

/// Minimum width of the sidebar's add menu.
const ADD_MENU_MIN_WIDTH: Pixels = px(200.0);

impl Sidebar {
    pub(super) fn render_add_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let sidebar_for_close = cx.entity().clone();

        let items = if self.active_tab == SidebarTab::Connections {
            self.add_connections_menu_items(cx)
        } else {
            self.add_scripts_menu_items(cx)
        };

        div()
            .absolute()
            .inset_0()
            .child(
                div()
                    .id("add-menu-overlay")
                    .absolute()
                    .inset_0()
                    .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                        sidebar_for_close.update(cx, |this, cx| {
                            this.close_add_menu(cx);
                        });
                    }),
            )
            .child(
                menu_frame(cx)
                    .absolute()
                    .top(datatab_components::tokens::ShellMetrics::SIDEBAR_HEADER_HEIGHT)
                    .right(Spacing::XS)
                    .min_w(ADD_MENU_MIN_WIDTH)
                    .children(items),
            )
    }

    /// One row of the sidebar's add menu; clicking closes the menu first.
    fn add_menu_row(
        &self,
        id: &'static str,
        icon: AppIcon,
        label: String,
        on_select: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let sidebar = cx.entity().clone();

        menu_row(id, &MenuItem::new(label).icon(icon), false, cx)
            .on_click(move |_, _, cx| {
                sidebar.update(cx, |this, cx| {
                    this.close_add_menu(cx);
                    on_select(this, cx);
                });
            })
            .into_any_element()
    }

    fn add_connections_menu_items(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            self.add_menu_row(
                "add-folder-option",
                AppIcon::Folder,
                datatab_i18n::t!("sidebar.overlay.add_folder"),
                |this, cx| this.create_root_folder(cx),
                cx,
            ),
            self.add_menu_row(
                "add-connection-option",
                AppIcon::Plug,
                datatab_i18n::t!("sidebar.overlay.add_connection"),
                |_, cx| cx.emit(SidebarEvent::RequestOpenConnectionManager),
                cx,
            ),
        ]
    }

    fn add_scripts_menu_items(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        vec![
            self.add_menu_row(
                "add-script-file",
                AppIcon::ScrollText,
                datatab_i18n::t!("sidebar.overlay.add_script_file"),
                |this, cx| this.create_script_file(cx),
                cx,
            ),
            self.add_menu_row(
                "add-script-folder",
                AppIcon::Folder,
                datatab_i18n::t!("sidebar.overlay.add_script_folder"),
                |this, cx| this.create_script_folder(cx),
                cx,
            ),
            self.add_menu_row(
                "import-script",
                AppIcon::Download,
                datatab_i18n::t!("sidebar.overlay.import_file"),
                |this, cx| this.import_script(cx),
                cx,
            ),
            self.add_menu_row(
                "add-external-scripts-folder",
                AppIcon::HardDrive,
                datatab_i18n::t!("sidebar.overlay.add_external_folder"),
                |this, cx| this.add_external_scripts_folder(cx),
                cx,
            ),
        ]
    }
}
