use super::*;
use std::path::PathBuf;

struct DataFolder(PathBuf);

impl DataFolder {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("datatab-socket-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path).expect("create isolated data folder");
        Self(path)
    }

    fn name(&self, debug: bool) -> String {
        app_control_name_for_data_dir(&self.0, debug).expect("resolve socket name")
    }
}

impl Drop for DataFolder {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove isolated data folder");
    }
}

#[test]
fn public_socket_name_uses_storage_data_folder() {
    let folder = datatab_storage::paths::data_dir().expect("resolve isolated test data folder");
    let expected = app_control_name_for_data_dir(&folder, cfg!(debug_assertions))
        .expect("resolve expected name")
        .to_ns_name::<GenericNamespaced>()
        .expect("convert expected name");
    assert_eq!(socket_name().expect("public name"), expected);
}

#[test]
fn different_data_folders_have_different_names() {
    let first = DataFolder::new();
    let second = DataFolder::new();
    assert_ne!(first.name(true), second.name(true));
    assert_ne!(first.name(false), second.name(false));
}

#[test]
fn same_folder_and_trailing_separator_have_same_name() {
    let folder = DataFolder::new();
    let mut spelling = folder.0.as_os_str().to_owned();
    spelling.push(std::path::MAIN_SEPARATOR.to_string());
    assert_eq!(folder.name(true), folder.name(true));
    assert_eq!(
        folder.name(true),
        app_control_name_for_data_dir(Path::new(&spelling), true).expect("trailing separator")
    );
}

#[cfg(unix)]
#[test]
fn symlink_to_same_folder_has_same_name() {
    let folder = DataFolder::new();
    let links = DataFolder::new();
    let link = links.0.join("alias");
    std::os::unix::fs::symlink(&folder.0, &link).expect("create symlink");
    assert_eq!(
        folder.name(true),
        app_control_name_for_data_dir(&link, true).expect("symlink name")
    );
}

#[test]
fn debug_suffix_and_platform_length_limits() {
    let folder = DataFolder::new();
    let long_path = folder.0.join("long-folder-name-".repeat(6));
    std::fs::create_dir(&long_path).expect("create long data path");
    let release = folder.name(false);
    let debug = app_control_name_for_data_dir(&long_path, true).expect("long-path name");
    assert_eq!(folder.name(true), release.replace(".sock", "-debug.sock"));
    assert!(debug.starts_with("datatab-"));
    assert_eq!(debug.len(), 35);
    assert_eq!(release.len(), 29);
    assert!(debug.len() < 108, "Linux abstract address and NUL");
    assert!("/tmp/".len() + debug.len() < 104, "macOS path and NUL");
    assert!(r"\\.\pipe\".len() + debug.len() <= 256, "Windows pipe");
}

#[test]
fn stable_hash_matches_fnv1a_vectors() {
    assert_eq!(fnv1a(b"".iter().copied()), 0xcbf29ce484222325);
    assert_eq!(fnv1a(b"hello".iter().copied()), 0xa430d84680aabd0b);
    #[cfg(unix)]
    assert_eq!(stable_path_hash(Path::new("/tmp")), 0x7f7de39c4bcebddb);
}

#[cfg(unix)]
#[test]
fn non_utf8_folders_have_distinct_names() {
    use std::os::unix::ffi::OsStrExt;
    let folder = DataFolder::new();
    let first = folder.0.join(std::ffi::OsStr::from_bytes(b"profile-\xff"));
    let second = folder.0.join(std::ffi::OsStr::from_bytes(b"profile-\xfe"));
    std::fs::create_dir(&first).expect("first non-UTF-8 folder");
    std::fs::create_dir(&second).expect("second non-UTF-8 folder");
    assert_ne!(
        app_control_name_for_data_dir(&first, true).expect("first name"),
        app_control_name_for_data_dir(&second, true).expect("second name")
    );
}

#[cfg(target_os = "linux")]
#[test]
fn unresolved_folder_uses_legacy_name_and_warns_once() {
    let folder = DataFolder::new();
    let blocker = folder.0.join("not-a-directory");
    std::fs::write(&blocker, b"").expect("create data path blocker");
    let output = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", "socket::tests::fallback_probe", "--nocapture"])
        .env("DATATAB_SOCKET_FALLBACK_PROBE", "1")
        .env("XDG_DATA_HOME", &blocker)
        .output()
        .expect("run fallback test in isolated process");
    assert!(output.status.success(), "fallback probe: {output:?}");
    let stderr = String::from_utf8(output.stderr).expect("warning text");
    assert_eq!(
        stderr
            .matches("Could not resolve app-control data folder")
            .count(),
        1
    );
}

#[cfg(target_os = "linux")]
#[test]
fn fallback_probe() {
    if std::env::var_os("DATATAB_SOCKET_FALLBACK_PROBE").is_none() {
        return;
    }
    let suffix = if cfg!(debug_assertions) { "-debug" } else { "" };
    let expected = format!("datatab{suffix}.sock")
        .to_ns_name::<GenericNamespaced>()
        .expect("fallback name");
    for _ in 0..3 {
        assert_eq!(socket_name().expect("fallback socket name"), expected);
    }
}

#[test]
fn canonicalization_failure_uses_legacy_name() {
    let folder = DataFolder::new();
    let missing = folder.0.join("missing");
    assert_eq!(
        app_control_name(Ok(missing.clone()), true),
        "datatab-debug.sock"
    );
    assert_eq!(app_control_name(Ok(missing), false), "datatab.sock");
}
