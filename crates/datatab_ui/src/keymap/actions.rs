use gpui::actions;

actions!(
    datatab,
    [
        ToggleCommandPalette,
        NewQueryTab,
        CloseCurrentTab,
        NextTab,
        PrevTab,
        SwitchToTab1,
        SwitchToTab2,
        SwitchToTab3,
        SwitchToTab4,
        SwitchToTab5,
        SwitchToTab6,
        SwitchToTab7,
        SwitchToTab8,
        SwitchToTab9,
        FocusSidebar,
        FocusEditor,
        FocusResults,
        FocusBackgroundTasks,
        CycleFocusForward,
        CycleFocusBackward,
        RunQuery,
        RunQueryInNewTab,
        ExportResults,
        OpenConnectionManager,
        Disconnect,
        RefreshSchema,
        ToggleEditor,
        ToggleResults,
        ToggleTasks,
        ToggleSidebar,
        // List navigation (SelectNext/SelectPrev/Execute/SelectFirst/SelectLast live in
        // datatab_components::actions so datatab_ui_document can reach them without an
        // upward dependency on datatab_ui)
        SelectFirst,
        SelectLast,
        ExpandCollapse,
        // Column navigation (Results)
        ColumnLeft,
        ColumnRight,
        // Directional panel navigation
        FocusLeft,
        FocusRight,
        FocusUp,
        FocusDown,
        // Settings
        OpenSettings,
        // Item menu
        OpenItemMenu,
        // Results toolbar
        FocusToolbar,
        TogglePanel,
        // File operations
        OpenScriptFile,
        SaveFileAs,
    ]
);

// Re-export shared document/navigation actions from datatab_components so
// callers inside datatab_ui can keep using `crate::keymap::SelectNext` etc.
pub use datatab_components::actions::{
    Delete, Execute, FocusSearch, Rename, SaveQuery, SelectNext, SelectPrev, ToggleFavorite,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_module_compiles() {
        let _action = ToggleCommandPalette;
    }
}
