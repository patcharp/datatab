use std::path::Path;

pub(super) fn display_path(path: &Path) -> String {
    let display = path.to_string_lossy();
    if let Some(unc) = display.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(normal) = display.strip_prefix(r"\\?\") {
        normal.to_string()
    } else {
        display.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::display_path;
    use std::path::Path;

    #[test]
    fn removes_extended_windows_prefix_for_display() {
        for (path, expected) in [
            (r"\\?\C:\Users\ana\query.sql", r"C:\Users\ana\query.sql"),
            (
                r"\\?\UNC\server\share\query.sql",
                r"\\server\share\query.sql",
            ),
            ("/tmp/query.sql", "/tmp/query.sql"),
        ] {
            assert_eq!(display_path(Path::new(path)), expected);
        }
    }
}
