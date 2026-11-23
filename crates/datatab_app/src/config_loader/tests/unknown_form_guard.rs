use super::*;
use crate::config_loader::{ProfileSaveError, save_profiles_preserving_value_refs};

#[test]
fn direct_save_unknown_form_refuses_new_literals_before_any_write() {
    let runtime = StorageRuntime::in_memory().expect("SQLite store");
    let mut profile = ConnectionProfile::new("unknown form", DbConfig::default_sqlite());
    profile.value_refs.insert(
        "extra_field".into(),
        datatab_core::ValueRef::literal("new-literal-440"),
    );
    let other = ConnectionProfile::new("must not be written", DbConfig::default_sqlite());
    let unknown_form_ids = [profile.id].into_iter().collect();
    let result = save_profiles(&runtime, &[other, profile], &unknown_form_ids);
    assert!(
        matches!(
            result,
            Err(ProfileSaveError::UnknownFormLiteral { profiles: 1 })
        ),
        "direct saves must refuse unknown-form literals, got {result:?}"
    );
    assert!(
        runtime
            .connection_profiles()
            .all()
            .expect("profiles")
            .is_empty()
    );
}

#[test]
fn known_form_broken_ref_preservation_does_not_refuse_new_literals() {
    let runtime = StorageRuntime::in_memory().expect("SQLite store");
    let mut profile = ConnectionProfile::new("known form", DbConfig::default_sqlite());
    profile
        .value_refs
        .insert("host".into(), datatab_core::ValueRef::env("UNUSED_HOST"));
    save_profiles(
        &runtime,
        std::slice::from_ref(&profile),
        &std::collections::HashSet::new(),
    )
    .expect("seed profile");
    runtime
        .open_datatab_db()
        .expect("database")
        .execute(
            "UPDATE cfg_connection_profile_value_refs SET ref_kind = 'retired-kind'",
            [],
        )
        .expect("break source kind");
    let loaded = load_config(&runtime).expect("load broken ref");
    assert_eq!(loaded.value_ref_diagnostics[&profile.id].len(), 1);
    let before = runtime
        .connection_profiles()
        .value_refs()
        .get_for_profile(&profile.id.to_string())
        .expect("stored refs");
    profile.name = "renamed".into();
    profile.value_refs.insert(
        "extra_field".into(),
        datatab_core::ValueRef::literal("new-literal-440"),
    );
    save_profiles_preserving_value_refs(
        &runtime,
        std::slice::from_ref(&profile),
        &loaded.value_ref_diagnostics,
        &std::collections::HashSet::new(),
    )
    .expect("known-form edit must save");
    assert_eq!(
        load_config(&runtime).expect("reload").profiles[0].name,
        "renamed"
    );
    assert_eq!(
        serde_json::to_value(
            runtime
                .connection_profiles()
                .value_refs()
                .get_for_profile(&profile.id.to_string())
                .expect("stored refs")
        )
        .expect("rows"),
        serde_json::to_value(before).expect("rows")
    );
}
