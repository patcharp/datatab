//! Cleanup after a cancelled query, run off the UI thread.
//!
//! A driver's `cleanup_after_cancel` can block: SQL Server recovery opens a
//! network connection and waits for a side-channel KILL. Every caller in this
//! crate goes through [`cleanup_after_cancel_in_background`], so a slow
//! reconnect never freezes the window.

use std::sync::Arc;

use datatab_core::Connection;
use gpui::AsyncApp;

/// Run `connection.cleanup_after_cancel()` on the background executor and wait
/// for it. Call it from the foreground task after the cancelled result has been
/// discarded, never from inside `cx.update`. A failure is logged under `label`.
pub(crate) async fn cleanup_after_cancel_in_background(
    connection: Arc<dyn Connection>,
    label: &'static str,
    cx: &AsyncApp,
) {
    let outcome = cx
        .background_executor()
        .spawn(async move { connection.cleanup_after_cancel() })
        .await;
    if let Err(error) = outcome {
        log::warn!("{label} cleanup after cancel failed: {error}");
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    const CRATES_DIR: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/..");

    fn collect_rust_files(root: &Path, out: &mut Vec<PathBuf>) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                collect_rust_files(&path, out);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                out.push(path);
            }
        }
    }

    #[test]
    fn cancel_cleanup_is_only_called_through_the_background_helper() {
        // Every `datatab_ui*` crate: the rule is about the UI thread, not
        // about one crate.
        let mut files = Vec::new();
        let crates = fs::read_dir(CRATES_DIR).expect("crates directory");
        for entry in crates.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("datatab_ui") {
                collect_rust_files(&entry.path().join("src"), &mut files);
            }
        }

        let mut offenders = Vec::new();
        for path in files {
            let path_str = path.to_string_lossy().to_string();
            if path_str.ends_with("cancel_cleanup.rs") {
                continue;
            }
            let Ok(source) = fs::read_to_string(&path) else {
                continue;
            };
            for (index, line) in source.lines().enumerate() {
                if line.contains(".cleanup_after_cancel()") && !path_str.contains("/tests") {
                    offenders.push(format!("{path_str}:{}", index + 1));
                }
            }
        }

        assert!(
            offenders.is_empty(),
            "call `cleanup_after_cancel_in_background` instead of `cleanup_after_cancel()` on the UI thread: {offenders:?}"
        );
    }
}
