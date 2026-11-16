use std::path::{Path, PathBuf};

/// Remove Windows drive and UNC verbatim prefixes from a path.
pub fn simplify_path(path: &Path) -> PathBuf {
    let display = path.to_string_lossy();
    if let Some(unc) = display.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{unc}"))
    } else if let Some(normal) = display.strip_prefix(r"\\?\").filter(|normal| {
        matches!(normal.as_bytes(), [drive, b':', b'\\', ..] if drive.is_ascii_alphabetic())
    }) {
        PathBuf::from(normal)
    } else {
        path.to_path_buf()
    }
}

/// A path as the user should read it: the extended-length form Windows
/// canonicalisation returns never reaches a message, a header or a tooltip.
pub(crate) fn display_path(path: &Path) -> String {
    simplify_path(path).to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::{display_path, simplify_path};
    use std::path::{Path, PathBuf};

    #[test]
    fn simplify_display_path_preserves_plain_paths_and_removes_verbatim_prefixes() {
        for (path, expected) in [
            (r"\\?\C:\x.sql", r"C:\x.sql"),
            (r"\\?\UNC\server\share\x.sql", r"\\server\share\x.sql"),
            (
                r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\x.sql",
                r"\\?\Volume{01234567-89ab-cdef-0123-456789abcdef}\x.sql",
            ),
            ("/tmp/x.sql", "/tmp/x.sql"),
            (r"C:\x.sql", r"C:\x.sql"),
        ] {
            assert_eq!(simplify_path(Path::new(path)), PathBuf::from(expected));
        }
    }

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
