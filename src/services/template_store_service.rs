// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable, app-owned mail templates.
//
// Templates contain only text/HTML authored by the user. They are kept apart
// from drafts so inserting or deleting a template can never mutate a draft or
// an original Evolution/EDS file.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailTemplate {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub subject: String,
    pub body_html: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct TemplateFile {
    version: u32,
    #[serde(default)]
    templates: Vec<MailTemplate>,
}

#[derive(Debug, Clone)]
pub struct TemplateStore {
    path: PathBuf,
}

impl TemplateStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        let path = std::env::var_os("MERIDIAN_TEMPLATE_FILE").or_else(|| std::env::var_os("EVOLUTION_GTK4_TEMPLATE_FILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| crate::services::app_data_dir().join("mail-templates.json"));
        Self::new(path)
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Vec<MailTemplate> {
        let Ok(raw) = fs::read(&self.path) else {
            return Vec::new();
        };
        let Ok(file) = serde_json::from_slice::<TemplateFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid mail template file");
            return Vec::new();
        };
        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported mail template version"
            );
            return Vec::new();
        }
        file.templates
    }

    pub fn upsert(&self, template: MailTemplate) -> io::Result<()> {
        if template.id.trim().is_empty() || template.name.trim().is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Vorlage benötigt eine ID und einen Namen",
            ));
        }
        let mut templates = self.load();
        if let Some(existing) = templates.iter_mut().find(|item| item.id == template.id) {
            *existing = template;
        } else {
            templates.push(template);
        }
        self.save(&templates)
    }

    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let mut templates = self.load();
        let original_len = templates.len();
        templates.retain(|template| template.id != id);
        if templates.len() == original_len {
            return Ok(false);
        }
        self.save(&templates)?;
        Ok(true)
    }

    fn save(&self, templates: &[MailTemplate]) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = TemplateFile {
            version: CURRENT_VERSION,
            templates: templates.to_vec(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)
    }
}

impl Default for TemplateStore {
    fn default() -> Self {
        Self::from_environment()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates_roundtrip_and_can_be_deleted() {
        let root =
            std::env::temp_dir().join(format!("meridian-templates-{}", uuid::Uuid::new_v4()));
        let store = TemplateStore::new(root.join("templates.json"));
        let template = MailTemplate {
            id: "template-1".to_string(),
            name: "Begrüßung".to_string(),
            subject: "Willkommen".to_string(),
            body_html: "<p>Hallo</p>".to_string(),
        };
        store
            .upsert(template.clone())
            .expect("template should save");
        assert_eq!(store.load(), vec![template]);
        assert!(store.delete("template-1").expect("template should delete"));
        assert!(store.load().is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn empty_template_name_is_rejected() {
        let store =
            TemplateStore::new(std::env::temp_dir().join("meridian-template-test.json"));
        let result = store.upsert(MailTemplate {
            id: "id".to_string(),
            name: " ".to_string(),
            subject: String::new(),
            body_html: String::new(),
        });
        assert_eq!(
            result.expect_err("empty name must fail").kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
