use super::*;

fn switch(
    setting_enabled: bool,
    env_value: Option<&str>,
    managed_file_found: bool,
) -> ReportSwitch {
    ReportSwitch::new(
        setting_enabled,
        env_value.map(str::to_string),
        managed_file_found,
    )
}

#[test]
fn every_source_alone_turns_reports_off() {
    assert_eq!(
        switch(false, None, false).state(),
        SwitchState::Disabled(DisableSource::Setting)
    );
    assert_eq!(
        switch(true, Some("1"), false).state(),
        SwitchState::Disabled(DisableSource::Environment)
    );
    assert_eq!(
        switch(true, None, true).state(),
        SwitchState::Disabled(DisableSource::ManagedFile)
    );
}

#[test]
fn an_organizations_sources_outrank_the_user_setting() {
    assert_eq!(
        switch(true, Some("1"), true).state(),
        SwitchState::Disabled(DisableSource::ManagedFile)
    );
    assert_eq!(
        switch(false, Some("1"), true).state(),
        SwitchState::Disabled(DisableSource::ManagedFile)
    );
    assert_eq!(
        switch(false, Some("true"), false).state(),
        SwitchState::Disabled(DisableSource::Environment)
    );
}

#[test]
fn nothing_can_force_reports_on() {
    assert_eq!(switch(true, None, false).state(), SwitchState::Enabled);

    for value in ["", "0", "false", "off", "no", " FALSE "] {
        assert_eq!(
            switch(true, Some(value), false).state(),
            SwitchState::Enabled,
            "{value:?} must not disable reports"
        );
    }
}

#[test]
fn an_unrecognized_environment_value_fails_closed() {
    for value in ["1", "true", "yes", "on", "disabled", "maybe"] {
        assert!(env_value_disables(value), "{value:?} must disable reports");
    }
}

#[test]
fn the_managed_file_lives_in_the_data_dir_and_the_system_path() {
    let data_dir = std::path::Path::new("/tmp/datatab-data");
    let paths = managed_file_paths(data_dir);

    assert_eq!(paths[0], data_dir.join(DISABLE_FILE_NAME));

    #[cfg(target_os = "linux")]
    assert!(paths.contains(&std::path::PathBuf::from(
        "/etc/datatab/error-reports-disabled"
    )));
}

#[test]
fn the_windows_machine_wide_file_is_probed() {
    let data_dir = std::path::Path::new("/tmp/datatab-data");
    let program_data = std::ffi::OsStr::new("/programdata");

    let paths = super::managed_file_paths_with(data_dir, Some(program_data));

    assert_eq!(
        paths.last(),
        Some(&std::path::PathBuf::from(
            "/programdata/DataTab/error-reports-disabled"
        )),
        "an administrator must be able to turn reports off for every user"
    );
}

#[test]
fn no_windows_base_means_no_extra_path() {
    let data_dir = std::path::Path::new("/tmp/datatab-data");

    let without = super::managed_file_paths_with(data_dir, None);
    let with = super::managed_file_paths_with(data_dir, Some(std::ffi::OsStr::new("/programdata")));

    assert_eq!(with.len(), without.len() + 1);
}

#[test]
fn from_process_reads_the_real_environment_and_files() {
    let dir = tempfile::TempDir::new().expect("temp dir");
    let file = dir.path().join(DISABLE_FILE_NAME);

    let absent = ReportSwitch::from_process(true, &[file.clone()]);
    assert!(!absent.managed_file_found);

    std::fs::write(&file, b"").expect("managed file written");
    let present = ReportSwitch::from_process(true, &[file]);

    assert!(present.managed_file_found);
    assert!(
        !present.state().is_enabled(),
        "the managed file must disable reports whatever the environment says"
    );
}
