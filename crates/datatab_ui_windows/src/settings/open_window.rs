//! Centralized entry point for opening (or focusing) the Settings window.
//!
//! Only one Settings window may exist at a time. All call sites should go
//! through [`open_or_focus_settings`] so the singleton invariant is preserved
//! regardless of which UI surface (workspace, sidebar footer, connection
//! manager) triggers the action.

use datatab_ui_base::{AppStateEntity, platform};
use gpui::*;
use gpui_component::Root;

use std::collections::HashMap;
use std::rc::Rc;

use super::{SettingsCoordinator, SettingsEvent, SettingsSectionId};

/// What the host of an app state does with the events of its Settings window.
pub type SettingsEventForwarder = Rc<dyn Fn(&SettingsEvent, &mut App)>;

#[derive(Default)]
struct SettingsEventForwarders(HashMap<EntityId, SettingsEventForwarder>);

impl Global for SettingsEventForwarders {}

/// Sets the forwarder every Settings window of `app_state` gets, whichever
/// surface opens it. A later call replaces the earlier forwarder; windows that
/// are already open keep the one they were given.
pub fn set_settings_event_forwarder(
    app_state: &Entity<AppStateEntity>,
    forwarder: SettingsEventForwarder,
    cx: &mut App,
) {
    cx.default_global::<SettingsEventForwarders>()
        .0
        .insert(app_state.entity_id(), forwarder);
}

/// Initial Settings window size, from the P1Settings* boards. Shrunk to the
/// display when it does not fit.
const WINDOW_WIDTH: f32 = 1320.0;
const WINDOW_HEIGHT: f32 = 900.0;

/// Smallest Settings window; the navigation and the pages scroll below the
/// initial size.
const WINDOW_MIN_WIDTH: f32 = 800.0;
const WINDOW_MIN_HEIGHT: f32 = 600.0;

/// Opens the Settings window or focuses it if one is already open.
///
/// When a window already exists, focuses it and returns without invoking
/// `setup`. The `section` parameter is only honored when opening a new
/// window — existing windows keep whichever section the user was on.
///
/// The forwarder set by [`set_settings_event_forwarder`] attaches to every
/// new window, whichever surface opens it, and carries the window's events
/// (`OpenScript`, `OpenLoginModal`) to the host. `setup` receives the new
/// coordinator and `&mut App` and is for window-local setup only: do not
/// forward events from it.
pub fn open_or_focus_settings<S>(
    app_state: Entity<AppStateEntity>,
    section: Option<SettingsSectionId>,
    cx: &mut App,
    setup: S,
) where
    S: FnOnce(&Entity<SettingsCoordinator>, &mut App) + 'static,
{
    if let Some(handle) = app_state.read(cx).settings_window {
        match handle.update(cx, |_root, window, _cx| {
            window.activate_window();
        }) {
            Ok(()) => return,
            Err(e) => {
                log::warn!("Stale settings window handle, reopening: {:?}", e);
                app_state.update(cx, |state, _| {
                    state.settings_window = None;
                });
            }
        }
    }

    let bounds = platform::fitted_window_bounds(WINDOW_WIDTH, WINDOW_HEIGHT, cx);

    let mut options = WindowOptions {
        app_id: Some(datatab_core::ReleaseChannel::current().app_id().into()),
        titlebar: Some(TitlebarOptions {
            title: Some(datatab_i18n::t!("connection_manager.tab.settings").into()),
            ..Default::default()
        }),
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        focus: true,
        ..Default::default()
    };
    platform::apply_window_options(&mut options, WINDOW_MIN_WIDTH, WINDOW_MIN_HEIGHT);
    platform::apply_shell_titlebar_options(&mut options);

    let app_state_for_new = app_state.clone();
    let app_state_id = app_state.entity_id();

    let open_result = cx.open_window(options, move |window, cx| {
        datatab_ui_base::ui_automation::install(window, cx);

        let settings = cx.new(|cx| match section {
            Some(section) => {
                SettingsCoordinator::new_with_section(app_state_for_new, section, window, cx)
            }
            None => SettingsCoordinator::new(app_state_for_new, window, cx),
        });

        setup(&settings, cx);

        let forwarder = cx
            .try_global::<SettingsEventForwarders>()
            .and_then(|forwarders| forwarders.0.get(&app_state_id).cloned());
        if let Some(forwarder) = forwarder {
            cx.subscribe(&settings, move |_settings, event: &SettingsEvent, cx| {
                forwarder(event, cx);
            })
            .detach();
        }

        cx.new(|cx| Root::new(settings, window, cx))
    });

    let Ok(handle) = open_result else {
        return;
    };

    app_state.update(cx, |state, _| {
        state.settings_window = Some(handle);
    });

    if let Err(e) = handle.update(cx, |_root, window, cx| {
        window.activate_window();
        cx.notify();
    }) {
        log::warn!("Failed to activate settings window: {:?}", e);
    }
}

#[cfg(test)]
mod window_title_i18n_tests {
    #[test]
    fn settings_window_title_key_resolves() {
        let value = datatab_i18n::t!("connection_manager.tab.settings", locale = "en");

        assert!(
            !value.is_empty(),
            "connection_manager.tab.settings resolved empty"
        );
    }
}

#[cfg(test)]
mod forwarder_tests {
    use super::{SettingsCoordinator, SettingsEvent, SettingsSectionId};
    use super::{open_or_focus_settings, set_settings_event_forwarder};
    use datatab_storage::bootstrap::StorageRuntime;
    use datatab_ui_base::AppStateEntity;
    use datatab_ui_base::keymap::init_keymap;
    use datatab_ui_base::toast::{ToastGlobal, ToastHost};
    use gpui::{AppContext as _, Entity, TestAppContext};
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    fn app_state(cx: &mut TestAppContext) -> Entity<AppStateEntity> {
        cx.update(gpui_component::init);
        cx.update(datatab_components::theme::init);
        cx.update(init_keymap);
        cx.update(|cx| {
            let host = cx.new(|_| ToastHost::new());
            cx.set_global(ToastGlobal { host });
        });
        cx.update(|cx| {
            cx.new(|_| {
                let runtime = StorageRuntime::in_memory().expect("in-memory storage");
                AppStateEntity::new_with_storage_runtime(runtime).expect("test storage setup")
            })
        })
    }

    type Slot = Rc<RefCell<Option<Entity<SettingsCoordinator>>>>;

    fn open(
        cx: &mut TestAppContext,
        app_state: &Entity<AppStateEntity>,
        section: Option<SettingsSectionId>,
    ) -> Slot {
        let slot = Slot::default();
        let slot_for_setup = slot.clone();
        cx.update(|cx| {
            open_or_focus_settings(app_state.clone(), section, cx, move |settings, _| {
                slot_for_setup.replace(Some(settings.clone()));
            });
        });
        slot
    }

    /// Settings opened for any section, or opened again while a window
    /// exists, forwards each event to the host exactly once.
    #[gpui::test]
    fn every_open_forwards_each_event_once(cx: &mut TestAppContext) {
        let app_state = app_state(cx);
        let count = Rc::new(Cell::new(0));
        let count_for_forwarder = count.clone();
        cx.update(|cx| {
            set_settings_event_forwarder(
                &app_state,
                Rc::new(move |_event: &SettingsEvent, _cx: &mut gpui::App| {
                    count_for_forwarder.set(count_for_forwarder.get() + 1);
                }),
                cx,
            );
        });

        let first = open(cx, &app_state, Some(SettingsSectionId::AuthProfiles));
        let second = open(cx, &app_state, None);
        assert!(
            second.borrow().is_none(),
            "the second open reuses the window"
        );

        let settings = first.borrow().clone().expect("the window opened");
        cx.update(|cx| {
            settings.update(cx, |_, cx| {
                cx.emit(SettingsEvent::OpenScript {
                    path: "a.lua".into(),
                });
            });
        });
        assert_eq!(count.get(), 1);
    }
}
