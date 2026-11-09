// SPDX-License-Identifier: GPL-3.0-or-later
//
// Quick Preview Service — GNOME Sushi (org.gnome.NautilusPreviewer) D-Bus
// integration with Flatpak-compatible XDG Desktop Portal (`gtk::FileLauncher`)
// and fallback to the system default application.

use gio::prelude::*;
use std::path::{Path, PathBuf};

/// Returns true when running inside a Flatpak sandbox.
pub fn is_running_in_flatpak() -> bool {
    Path::new("/.flatpak-info").exists() || std::env::var_os("FLATPAK_ID").is_some()
}

/// Returns a host-accessible cache directory for staging attachment files.
/// Inside Flatpak, `/tmp` is private to the sandbox and rejected by
/// `org.freedesktop.portal.OpenURI.OpenFile`, whereas `glib::user_cache_dir()`
/// (`~/.var/app/<app-id>/cache`) resides on the real host filesystem.
pub fn get_attachment_cache_dir() -> PathBuf {
    let cache_dir = glib::user_cache_dir().join("meridian").join("attachments");
    if std::fs::create_dir_all(&cache_dir).is_ok() {
        return cache_dir;
    }
    let fallback = std::env::temp_dir().join("meridian-attachments");
    let _ = std::fs::create_dir_all(&fallback);
    fallback
}

/// Sanitizes an attachment filename so it is safe to use as a single path component.
pub fn sanitize_attachment_filename(filename: &str) -> String {
    let trimmed = filename.trim();
    if trimmed.is_empty() {
        return "attachment".to_string();
    }
    let clean = Path::new(trimmed)
        .file_name()
        .and_then(|f| f.to_str())
        .unwrap_or(trimmed)
        .replace(['/', '\\', '\0'], "_");
    if clean.trim().is_empty() || clean == "." || clean == ".." {
        "attachment".to_string()
    } else {
        clean
    }
}

/// Writes an attachment payload into the host-accessible attachment cache directory.
pub fn write_attachment_to_cache(filename: &str, content: &[u8]) -> Result<PathBuf, String> {
    let cache_dir = get_attachment_cache_dir();
    let safe_name = sanitize_attachment_filename(filename);
    let path = cache_dir.join(safe_name);
    std::fs::write(&path, content).map_err(|e| format!("Could not write attachment cache file: {e}"))?;
    Ok(path)
}

/// Opens a local file on disk using internal handlers for calendar/contact files or
/// `gtk::FileLauncher` (XDG Desktop Portal `OpenURI.OpenFile` with file descriptor)
/// and fallback to `gio::AppInfo::launch_default_for_uri`.
pub fn open_file_with_system_viewer(
    path: &Path,
    parent_window: Option<&gtk::Window>,
) -> Result<(), String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        ext.as_str(),
        "ics" | "ical" | "ifb" | "vcs" | "vcf" | "vcard"
    ) {
        if let Some(app) = gio::Application::default()
            .and_downcast::<crate::application::MeridianApplication>()
        {
            app.handle_external_open(&[gio::File::for_path(path)]);
            return Ok(());
        }
    }

    let file = gio::File::for_path(path);
    if gtk::is_initialized() {
        let launcher = gtk::FileLauncher::new(Some(&file));
        let file_for_fallback = file.clone();
        launcher.launch(parent_window, gio::Cancellable::NONE, move |res| {
            if let Err(err) = res {
                tracing::warn!(%err, "FileLauncher failed; falling back to AppInfo::launch_default_for_uri");
                let uri = file_for_fallback.uri();
                if let Err(fallback_err) =
                    gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
                {
                    tracing::error!(%fallback_err, uri = %uri, "Could not open attachment file");
                }
            }
        });
        Ok(())
    } else {
        let uri = file.uri();
        gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
            .map_err(|e| format!("Could not open file: {e}"))
    }
}

/// Writes attachment bytes to the cache directory and opens the file in the default viewer.
pub fn open_attachment_bytes(
    filename: &str,
    content: &[u8],
    parent_window: Option<&gtk::Window>,
) -> Result<PathBuf, String> {
    let path = write_attachment_to_cache(filename, content)?;
    open_file_with_system_viewer(&path, parent_window)?;
    Ok(path)
}

/// Previews a file or URI using GNOME Sushi via D-Bus (`org.gnome.NautilusPreviewer2.ShowFile`),
/// falling back to the system's default application (`open_file_with_system_viewer` / `launch_default_for_uri`).
pub fn preview_file_or_uri(path_or_uri: &str, window_handle: Option<&str>) -> Result<(), String> {
    let file = if let Some(stripped) = path_or_uri.strip_prefix("file://") {
        let by_uri = gio::File::for_uri(path_or_uri);
        if by_uri.path().is_some_and(|p| p.exists()) {
            by_uri
        } else {
            gio::File::for_path(stripped)
        }
    } else if path_or_uri.contains("://") {
        gio::File::for_uri(path_or_uri)
    } else {
        gio::File::for_path(path_or_uri)
    };
    if let Some(path) = file.path() {
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if matches!(
            ext.as_str(),
            "ics" | "ical" | "ifb" | "vcs" | "vcf" | "vcard"
        ) {
            if let Some(app) = gio::Application::default()
                .and_downcast::<crate::application::MeridianApplication>()
            {
                app.handle_external_open(&[file]);
                return Ok(());
            }
        }
    }
    let uri = file.uri().to_string();

    // Attempt native GNOME Sushi Quick Look via D-Bus session when not sandboxed
    if !is_running_in_flatpak() {
        let sushi_res = (|| -> Result<(), Box<dyn std::error::Error>> {
            let bus = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE)?;
            let msg = gio::DBusMessage::new_method_call(
                Some("org.gnome.NautilusPreviewer"),
                "/org/gnome/NautilusPreviewer",
                Some("org.gnome.NautilusPreviewer2"),
                "ShowFile",
            );
            let handle = window_handle.unwrap_or("");
            msg.set_body(&(
                uri.as_str(),
                handle,
                true, // closeIfAlreadyShown: toggles close if already previewing this file
                "",
            ).to_variant());
            let _ = bus.send_message_with_reply_sync(
                &msg,
                gio::DBusSendMessageFlags::NONE,
                1500,
                gio::Cancellable::NONE,
            )?;
            Ok(())
        })();

        if sushi_res.is_ok() {
            return Ok(());
        }
    }

    if let Some(path) = file.path() {
        return open_file_with_system_viewer(&path, None);
    }

    // Fallback: launch default desktop viewer for non-file URIs
    gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
        .map_err(|e| format!("Could not open file: {e}"))
}

/// Ensures that a previewable file exists on disk for the given `DraftAttachment`.
/// If the attachment already points to an existing file, its path is returned.
/// If it contains embedded Base64 data, it is decoded into a temporary preview cache file.
pub fn ensure_attachment_preview_path(
    attachment: &crate::services::draft_store_service::DraftAttachment,
) -> Result<PathBuf, String> {
    let path = Path::new(&attachment.path);
    if path.exists() {
        return Ok(path.to_path_buf());
    }

    if !attachment.embedded_content_base64.is_empty() {
        if let Ok(bytes) = crate::services::mail_composer_service::decode_embedded_attachment(
            &attachment.embedded_content_base64,
        ) {
            if let Ok(cache_path) = write_attachment_to_cache(&attachment.filename, &bytes) {
                return Ok(cache_path);
            }
        }
    }

    if !attachment.path.is_empty() {
        Ok(PathBuf::from(&attachment.path))
    } else {
        Err("No file path or content available for preview".to_string())
    }
}

/// Previews a `DraftAttachment`. If the attachment is local on disk, previews it directly.
/// If it is embedded Base64 data without a persistent file on disk, decodes it into a
/// temporary preview cache file first.
pub fn preview_attachment(
    attachment: &crate::services::draft_store_service::DraftAttachment,
    window_handle: Option<&str>,
) -> Result<(), String> {
    let preview_path = ensure_attachment_preview_path(attachment)?;
    preview_file_or_uri(&preview_path.to_string_lossy(), window_handle)
}

/// Explicitly closes an open GNOME Sushi preview window.
pub fn close_preview() {
    if let Ok(bus) = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE) {
        let msg = gio::DBusMessage::new_method_call(
            Some("org.gnome.NautilusPreviewer"),
            "/org/gnome/NautilusPreviewer",
            Some("org.gnome.NautilusPreviewer2"),
            "Close",
        );
        let _ = bus.send_message(&msg, gio::DBusSendMessageFlags::NONE);
    }
}

/// Returns the user's standard Downloads directory.
pub fn get_downloads_directory() -> PathBuf {
    glib::user_special_dir(glib::UserDirectory::Downloads)
        .unwrap_or_else(|| glib::home_dir().join("Downloads"))
}

/// Checks whether `get_downloads_directory()` is backed by a real host filesystem
/// rather than an isolated Flatpak sandbox `tmpfs`.
pub fn is_downloads_dir_host_accessible() -> bool {
    if !is_running_in_flatpak() {
        return true;
    }
    let downloads = get_downloads_directory();
    let Ok(mountinfo) = std::fs::read_to_string("/proc/self/mountinfo") else {
        return true;
    };
    let mut best_match_len = 0usize;
    let mut best_fstype = String::new();
    for line in mountinfo.lines() {
        let Some((before_sep, after_sep)) = line.split_once(" - ") else {
            continue;
        };
        let mut before_parts = before_sep.split_whitespace();
        let Some(mount_point_raw) = before_parts.nth(4) else {
            continue;
        };
        let mount_point_str = mount_point_raw.replace("\\040", " ");
        let mount_path = Path::new(&mount_point_str);
        if downloads.starts_with(mount_path) {
            let len = mount_point_str.len();
            if len >= best_match_len {
                if let Some(fstype) = after_sep.split_whitespace().next() {
                    best_match_len = len;
                    best_fstype = fstype.to_string();
                }
            }
        }
    }
    !best_fstype.eq_ignore_ascii_case("tmpfs")
}

/// Resolves a safe, non-colliding file path in the given directory.
/// If `file.ext` already exists, produces `file (1).ext`, `file (2).ext`, etc.
pub fn resolve_unique_download_path(filename: &str) -> PathBuf {
    let dir = get_downloads_directory();
    let _ = std::fs::create_dir_all(&dir);
    let clean_name = sanitize_attachment_filename(filename);
    let initial_path = dir.join(&clean_name);
    if !initial_path.exists() {
        return initial_path;
    }
    let p = Path::new(&clean_name);
    let stem = p.file_stem().and_then(|s| s.to_str()).unwrap_or("attachment");
    let ext = p.extension().and_then(|e| e.to_str());
    for i in 1..10000 {
        let candidate = match ext {
            Some(e) => format!("{stem} ({i}).{e}"),
            None => format!("{stem} ({i})"),
        };
        let candidate_path = dir.join(&candidate);
        if !candidate_path.exists() {
            return candidate_path;
        }
    }
    initial_path
}

/// Directly downloads a single attachment to the user's Downloads folder without prompting.
pub fn download_attachment_to_downloads(filename: &str, content: &[u8]) -> Result<PathBuf, String> {
    if !is_downloads_dir_host_accessible() {
        return Err("Downloads directory is not exposed to the Flatpak sandbox".to_string());
    }
    let target = resolve_unique_download_path(filename);
    std::fs::write(&target, content).map_err(|e| format!("Could not write file: {e}"))?;
    Ok(target)
}

/// Directly downloads all attachments to the user's Downloads folder without prompting.
pub fn download_all_attachments_to_downloads(
    attachments: &[crate::models::mail_message::AttachmentInfo],
) -> Result<(usize, PathBuf), String> {
    if !is_downloads_dir_host_accessible() {
        return Err("Downloads directory is not exposed to the Flatpak sandbox".to_string());
    }
    let dir = get_downloads_directory();
    let _ = std::fs::create_dir_all(&dir);
    let mut count = 0;
    for att in attachments {
        let target = resolve_unique_download_path(&att.filename);
        if std::fs::write(&target, att.content.as_ref()).is_ok() {
            count += 1;
        }
    }
    Ok((count, dir))
}

/// Opens the specified directory (or reveals the specified file in its containing folder)
/// in the system's default file manager, using `gtk::FileLauncher::launch_containing_folder`
/// so it works inside Flatpak via `org.freedesktop.portal.OpenURI.OpenDirectory`.
pub fn open_folder_in_file_manager(folder_or_file: &Path) -> Result<(), String> {
    open_folder_in_file_manager_with_parent(folder_or_file, None)
}

/// Opens the specified directory or reveals the specified file in the file manager
/// with an optional parent `gtk::Window` for XDG Desktop Portal integration.
pub fn open_folder_in_file_manager_with_parent(
    folder_or_file: &Path,
    parent_window: Option<&gtk::Window>,
) -> Result<(), String> {
    let file = gio::File::for_path(folder_or_file);
    if gtk::is_initialized() {
        let launcher = gtk::FileLauncher::new(Some(&file));
        let folder_path = if folder_or_file.is_file() {
            folder_or_file
                .parent()
                .unwrap_or(folder_or_file)
                .to_path_buf()
        } else {
            folder_or_file.to_path_buf()
        };
        launcher.open_containing_folder(parent_window, gio::Cancellable::NONE, move |res| {
            if let Err(err) = res {
                tracing::warn!(%err, "open_containing_folder failed; falling back to launch_default_for_uri");
                let dir_uri = gio::File::for_path(&folder_path).uri();
                if let Err(fallback_err) =
                    gio::AppInfo::launch_default_for_uri(&dir_uri, None::<&gio::AppLaunchContext>)
                {
                    tracing::error!(%fallback_err, uri = %dir_uri, "Could not open folder in file manager");
                }
            }
        });
        Ok(())
    } else {
        let dir_path = if folder_or_file.is_file() {
            folder_or_file.parent().unwrap_or(folder_or_file)
        } else {
            folder_or_file
        };
        let uri = gio::File::for_path(dir_path).uri();
        gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
            .map_err(|e| format!("Could not open folder: {e}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::draft_store_service::DraftAttachment;

    #[test]
    fn test_close_preview_does_not_panic() {
        close_preview();
    }

    #[test]
    fn test_embedded_attachment_preview_writes_temp_file() {
        let dummy_text = "Hello Meridian Sushi Preview";
        let base64_data = crate::services::mail_composer_service::base64(dummy_text.as_bytes());
        let attachment = DraftAttachment {
            path: "".to_string(),
            filename: "test_sushi.txt".to_string(),
            mime_type: "text/plain".to_string(),
            size: dummy_text.len() as u64,
            content_id: None,
            embedded_content_base64: base64_data,
        };

        let cache_dir = get_attachment_cache_dir();
        let expected_file = cache_dir.join("test_sushi.txt");
        if expected_file.exists() {
            let _ = std::fs::remove_file(&expected_file);
        }

        // Test file preparation without triggering live D-Bus calls to active desktop session
        let path_res = ensure_attachment_preview_path(&attachment);
        assert!(path_res.is_ok(), "Preview path generation must succeed");
        assert!(expected_file.exists(), "Cache preview file must be written");
        let content = std::fs::read_to_string(&expected_file).unwrap();
        assert_eq!(content, dummy_text);
        let _ = std::fs::remove_file(&expected_file);
    }

    #[test]
    fn test_unique_download_path_resolution_and_download() {
        let unique_name = format!("test_download_{}.txt", glib::random_int());
        let content = b"attachment download test content";
        let target = download_attachment_to_downloads(&unique_name, content).expect("download succeeds");
        assert!(target.exists());
        assert_eq!(std::fs::read(&target).unwrap(), content);

        // Download again with same filename: should produce unique collision name
        let target2 = download_attachment_to_downloads(&unique_name, content).expect("download duplicate succeeds");
        assert!(target2.exists());
        assert_ne!(target, target2);
        assert!(target2.to_string_lossy().contains("(1)"));

        let _ = std::fs::remove_file(&target);
        let _ = std::fs::remove_file(&target2);
    }
}

