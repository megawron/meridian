// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable storage for the out-of-office configuration.
//
// The native server-side auto-responder operation remains an EDS/Camel
// integration task.  Keeping the non-secret configuration here means the
// portable UI does not lose user input while that backend is unavailable.

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutOfOfficeSettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "default_schedule")]
    pub use_schedule: bool,
    #[serde(default)]
    pub start_date: String,
    #[serde(default)]
    pub end_date: String,
    #[serde(default)]
    pub internal_message: String,
    #[serde(default)]
    pub external_message: String,
}

impl Default for OutOfOfficeSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            use_schedule: true,
            start_date: String::new(),
            end_date: String::new(),
            internal_message: String::new(),
            external_message: String::new(),
        }
    }
}

impl OutOfOfficeSettings {
    pub fn validate(&self) -> Result<(), String> {
        let start = parse_optional_date(&self.start_date, "Das Startdatum")?;
        let end = parse_optional_date(&self.end_date, "Das Enddatum")?;
        if let (Some(start), Some(end)) = (start, end) {
            if end < start {
                return Err("Das Enddatum muss am oder nach dem Startdatum liegen".to_string());
            }
        }
        if self.internal_message.chars().count() > 100_000
            || self.external_message.chars().count() > 100_000
        {
            return Err(
                "Die Abwesenheitsnachricht darf höchstens 100.000 Zeichen enthalten".to_string(),
            );
        }
        Ok(())
    }
}

fn default_schedule() -> bool {
    true
}

fn parse_optional_date(value: &str, label: &str) -> Result<Option<NaiveDate>, String> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .map(Some)
        .map_err(|_| format!("{label} muss dem Format YYYY-MM-DD entsprechen"))
}

#[derive(Debug, Clone)]
pub struct OutOfOfficeStore {
    path: PathBuf,
}

impl OutOfOfficeStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> OutOfOfficeSettings {
        let Ok(raw) = fs::read(&self.path) else {
            return OutOfOfficeSettings::default();
        };
        let Ok(file) = serde_json::from_slice::<OutOfOfficeFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid out-of-office settings");
            return OutOfOfficeSettings::default();
        };
        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported out-of-office settings version"
            );
            return OutOfOfficeSettings::default();
        }
        file.settings
    }

    pub fn save(&self, settings: &OutOfOfficeSettings) -> io::Result<()> {
        settings
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = OutOfOfficeFile {
            version: CURRENT_VERSION,
            settings: settings.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)
    }
}

impl Default for OutOfOfficeStore {
    fn default() -> Self {
        Self::new(crate::services::app_data_dir().join("out-of-office.json"))
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct OutOfOfficeFile {
    version: u32,
    settings: OutOfOfficeSettings,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_office_settings_roundtrip_is_versioned() {
        let root = std::env::temp_dir().join(format!(
            "evolution-gtk4-out-of-office-{}",
            uuid::Uuid::new_v4()
        ));
        let store = OutOfOfficeStore::new(root.join("out-of-office.json"));
        let settings = OutOfOfficeSettings {
            enabled: true,
            use_schedule: true,
            start_date: "2026-08-11".to_string(),
            end_date: "2026-08-20".to_string(),
            internal_message: "Bin im Urlaub".to_string(),
            external_message: "Out of office".to_string(),
        };

        store.save(&settings).expect("settings should save");
        assert_eq!(store.load(), settings);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn out_of_office_settings_reject_invalid_ranges() {
        let settings = OutOfOfficeSettings {
            start_date: "2026-08-20".to_string(),
            end_date: "2026-08-11".to_string(),
            ..OutOfOfficeSettings::default()
        };
        assert!(settings.validate().is_err());
    }
}
