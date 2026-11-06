// SPDX-License-Identifier: GPL-3.0-or-later
//
// Quick Preview Service — GNOME Sushi (org.gnome.NautilusPreviewer) D-Bus
// integration with fallback to the system default application.

use gio::prelude::*;
use std::path::{Path, PathBuf};

/// Previews a file or URI using GNOME Sushi via D-Bus (`org.gnome.NautilusPreviewer2.ShowFile`),
/// falling back to the system's default application (`gio::AppInfo::launch_default_for_uri`).
pub fn preview_file_or_uri(path_or_uri: &str, window_handle: Option<&str>) -> Result<(), String> {
    let file = if path_or_uri.starts_with("file://") || path_or_uri.contains("://") {
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

    // Attempt native GNOME Sushi Quick Look via D-Bus session
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

    // Fallback: launch default desktop viewer
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
            let temp_dir = std::env::temp_dir().join("meridian-previews");
            let _ = std::fs::create_dir_all(&temp_dir);
            let safe_filename = if attachment.filename.is_empty() {
                "attachment".to_string()
            } else {
                attachment
                    .filename
                    .replace(['/', '\\', '\0'], "_")
            };
            let temp_path: PathBuf = temp_dir.join(&safe_filename);
            if std::fs::write(&temp_path, &bytes).is_ok() {
                return Ok(temp_path);
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

/// Resolves a safe, non-colliding file path in the given directory.
/// If `file.ext` already exists, produces `file (1).ext`, `file (2).ext`, etc.
pub fn resolve_unique_download_path(filename: &str) -> PathBuf {
    let dir = get_downloads_directory();
    let _ = std::fs::create_dir_all(&dir);
    let clean_name = if filename.trim().is_empty() {
        "attachment".to_string()
    } else {
        Path::new(filename)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or("attachment")
            .replace(['/', '\\', '\0'], "_")
    };
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
    let target = resolve_unique_download_path(filename);
    std::fs::write(&target, content).map_err(|e| format!("Could not write file: {e}"))?;
    Ok(target)
}

/// Directly downloads all attachments to the user's Downloads folder without prompting.
pub fn download_all_attachments_to_downloads(
    attachments: &[crate::models::mail_message::AttachmentInfo],
) -> Result<(usize, PathBuf), String> {
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

/// Opens the specified directory in the system's default file manager.
pub fn open_folder_in_file_manager(folder: &Path) -> Result<(), String> {
    let uri = format!("file://{}", folder.to_string_lossy());
    gio::AppInfo::launch_default_for_uri(&uri, None::<&gio::AppLaunchContext>)
        .map_err(|e| format!("Could not open folder: {e}"))
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

        let temp_dir = std::env::temp_dir().join("meridian-previews");
        let expected_file = temp_dir.join("test_sushi.txt");
        if expected_file.exists() {
            let _ = std::fs::remove_file(&expected_file);
        }

        // Test file preparation without triggering live D-Bus calls to active desktop session
        let path_res = ensure_attachment_preview_path(&attachment);
        assert!(path_res.is_ok(), "Preview path generation must succeed");
        assert!(expected_file.exists(), "Temporary preview file must be written");
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

