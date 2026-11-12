use super::{AddMemberModal, KeyType};
use gpui::{AppContext as _, Modifiers, TestAppContext};

#[gpui::test]
fn delete_has_a_24_pixel_hit_area_and_add_still_works(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    cx.update(datatab_components::theme::init);
    let (modal, window) = cx.add_window_view(|window, cx| {
        let mut modal = AddMemberModal::new(cx);
        modal.open(KeyType::Hash, window, cx);
        modal
    });
    window.run_until_parked();
    let delete = window
        .debug_bounds("add-member-remove-row-0")
        .expect("delete is laid out");
    assert!(
        delete.size.width
            >= datatab_components::tokens::Heights::MIN_HIT_TARGET
                .to_pixels(gpui::px(datatab_components::tokens::BASE_REM)),
        "delete width is at least 24 px"
    );
    assert!(
        delete.size.height
            >= datatab_components::tokens::Heights::MIN_HIT_TARGET
                .to_pixels(gpui::px(datatab_components::tokens::BASE_REM)),
        "delete height is at least 24 px"
    );
    let add = window
        .debug_bounds("add-member-add-row")
        .expect("add is laid out");
    window.simulate_click(add.center(), Modifiers::default());
    assert_eq!(
        window.update(|_, cx| modal.read(cx).value_rows.len()),
        2,
        "add still adds a row"
    );
    window.simulate_click(delete.center(), Modifiers::default());
    assert_eq!(
        window.update(|_, cx| modal.read(cx).value_rows.len()),
        1,
        "delete removes only its row"
    );
}
