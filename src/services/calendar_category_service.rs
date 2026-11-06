// SPDX-License-Identifier: GPL-3.0-or-later
//
// Portable calendar category names and colors.
//
// iCalendar keeps the category names on each VEVENT/VTODO.  This small
// app-owned store contains only the presentation metadata (the color chosen
// by the user); EDS and imported calendar components remain untouched.

use serde::{Deserialize, Serialize};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

const CURRENT_VERSION: u32 = 1;

static CATEGORY_CACHE: Mutex<Option<(PathBuf, Instant, Option<SystemTime>, CategoryFile)>> =
    Mutex::new(None);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarCategory {
    pub id: String,
    pub name: String,
    pub color: String,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
struct CategoryFile {
    version: u32,
    #[serde(default)]
    categories: Vec<CalendarCategory>,
}

#[derive(Debug, Clone)]
pub struct CalendarCategoryStore {
    path: PathBuf,
}

impl CalendarCategoryStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn from_environment() -> Self {
        let path = std::env::var_os("MERIDIAN_CATEGORY_FILE")
            .or_else(|| std::env::var_os("EVOLUTION_GTK4_CATEGORY_FILE"))
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                if let Ok(guard) = CATEGORY_CACHE.lock() {
                    if let Some((ref cached_path, _, _, _)) = *guard {
                        return cached_path.clone();
                    }
                }
                crate::services::app_data_dir().join("calendar-categories.json")
            });
        Self::new(path)
    }

    #[allow(dead_code)]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Vec<CalendarCategory> {
        self.load_file().categories
    }

    pub fn color_for(&self, name: &str) -> Option<String> {
        let name = name.trim();
        if name.is_empty() {
            return None;
        }
        self.load()
            .into_iter()
            .find(|category| category.name.eq_ignore_ascii_case(name))
            .map(|category| category.color)
    }

    pub fn color_for_component(&self, component: &str) -> Option<String> {
        let names = category_names_from_ical_component(component);
        if names.is_empty() {
            return None;
        }
        let categories = self.load();
        if categories.is_empty() {
            return None;
        }
        names.into_iter().find_map(|name| {
            let name = name.trim();
            if name.is_empty() {
                return None;
            }
            categories
                .iter()
                .find(|category| category.name.eq_ignore_ascii_case(name))
                .map(|category| category.color.clone())
        })
    }

    pub fn upsert(&self, category: CalendarCategory) -> io::Result<()> {
        let category = normalize_category(category)?;
        let mut file = self.load_file();
        if file.categories.iter().any(|existing| {
            existing.id != category.id && existing.name.eq_ignore_ascii_case(&category.name)
        }) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "A calendar category with this name already exists",
            ));
        }
        if let Some(existing) = file
            .categories
            .iter_mut()
            .find(|existing| existing.id == category.id)
        {
            *existing = category;
        } else {
            file.categories.push(category);
        }
        sort_categories(&mut file.categories);
        self.save_file(&file)
    }

    pub fn ensure_names<I, S>(&self, names: I) -> io::Result<usize>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut file = self.load_file();
        let mut added = 0;
        for name in names {
            let name = name.as_ref().trim();
            if name.is_empty()
                || file
                    .categories
                    .iter()
                    .any(|category| category.name.eq_ignore_ascii_case(name))
            {
                continue;
            }
            let category = CalendarCategory {
                id: format!("category-{}-{}", slug(name), uuid::Uuid::new_v4()),
                name: name.to_string(),
                color: default_color(file.categories.len()),
            };
            file.categories.push(category);
            added += 1;
        }
        if added > 0 {
            sort_categories(&mut file.categories);
            self.save_file(&file)?;
        }
        Ok(added)
    }

    pub fn delete(&self, id: &str) -> io::Result<bool> {
        let mut file = self.load_file();
        let original_len = file.categories.len();
        file.categories.retain(|category| category.id != id);
        if original_len == file.categories.len() {
            return Ok(false);
        }
        self.save_file(&file)?;
        Ok(true)
    }

    fn load_file(&self) -> CategoryFile {
        if let Ok(guard) = CATEGORY_CACHE.lock() {
            if let Some((ref cached_path, last_checked, _, ref file)) = *guard {
                if cached_path == &self.path && last_checked.elapsed() < Duration::from_secs(2) {
                    return file.clone();
                }
            }
        }
        let now = Instant::now();
        let mtime = fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if let Ok(mut guard) = CATEGORY_CACHE.lock() {
            if let Some((ref cached_path, ref mut last_checked, ref cached_mtime, ref file)) = *guard {
                if cached_path == &self.path && *cached_mtime == mtime {
                    *last_checked = now;
                    return file.clone();
                }
            }
        }
        let Ok(raw) = fs::read(&self.path) else {
            let file = CategoryFile {
                version: CURRENT_VERSION,
                categories: default_categories(),
            };
            if let Ok(mut guard) = CATEGORY_CACHE.lock() {
                *guard = Some((self.path.clone(), now, mtime, file.clone()));
            }
            return file;
        };
        let Ok(mut file) = serde_json::from_slice::<CategoryFile>(&raw) else {
            tracing::warn!(path = %self.path.display(), "Ignoring invalid calendar category file");
            let file = CategoryFile {
                version: CURRENT_VERSION,
                categories: default_categories(),
            };
            if let Ok(mut guard) = CATEGORY_CACHE.lock() {
                *guard = Some((self.path.clone(), now, mtime, file.clone()));
            }
            return file;
        };
        if file.version != CURRENT_VERSION {
            tracing::warn!(
                path = %self.path.display(),
                version = file.version,
                expected = CURRENT_VERSION,
                "Ignoring unsupported calendar category version"
            );
            let file = CategoryFile {
                version: CURRENT_VERSION,
                categories: default_categories(),
            };
            if let Ok(mut guard) = CATEGORY_CACHE.lock() {
                *guard = Some((self.path.clone(), now, mtime, file.clone()));
            }
            return file;
        }
        file.categories.retain(|category| {
            !category.id.trim().is_empty()
                && !category.name.trim().is_empty()
                && valid_color(&category.color)
        });
        sort_categories(&mut file.categories);
        if let Ok(mut guard) = CATEGORY_CACHE.lock() {
            *guard = Some((self.path.clone(), now, mtime, file.clone()));
        }
        file
    }

    fn save_file(&self, file: &CategoryFile) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = CategoryFile {
            version: CURRENT_VERSION,
            categories: file.categories.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&file)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
        let temporary_path = self.path.with_extension("json.tmp");
        fs::write(&temporary_path, encoded)?;
        fs::rename(temporary_path, &self.path)?;
        let mtime = fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        if let Ok(mut guard) = CATEGORY_CACHE.lock() {
            *guard = Some((self.path.clone(), Instant::now(), mtime, file));
        }
        Ok(())
    }
}

impl Default for CalendarCategoryStore {
    fn default() -> Self {
        Self::from_environment()
    }
}

/// Extract RFC5545 `CATEGORIES` values while respecting escaped commas inside
/// one category name.  This is shared by VEVENT, VTODO, and VJOURNAL views.
pub fn category_names_from_ical_component(raw: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current_match: Option<String> = None;
    for raw_line in raw.split('\n') {
        let raw_line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if raw_line.starts_with(' ') || raw_line.starts_with('\t') {
            if let Some(prev) = current_match.as_mut() {
                prev.push_str(raw_line.trim_start());
            }
            continue;
        }
        if let Some(unfolded) = current_match.take() {
            if let Some((property, value)) = unfolded.split_once(':') {
                let name = property.split(';').next().unwrap_or(property);
                if name.eq_ignore_ascii_case("CATEGORIES") {
                    values.extend(category_names_from_ical_value(value));
                }
            }
        }
        if raw_line.starts_with("X-EWS-ORIGINAL-COMP:") {
            break;
        }
        if raw_line.len() >= 10 && raw_line.as_bytes()[0].eq_ignore_ascii_case(&b'C') {
            let first_token = raw_line.split([':', ';']).next().unwrap_or(raw_line);
            if first_token.eq_ignore_ascii_case("CATEGORIES") {
                current_match = Some(raw_line.to_string());
            }
        } else if !raw_line.is_empty()
            && raw_line.as_bytes()[0].eq_ignore_ascii_case(&b'C')
            && !raw_line.contains([':', ';'])
            && "CATEGORIES"
                .get(..raw_line.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(raw_line))
        {
            current_match = Some(raw_line.to_string());
        }
    }
    if let Some(unfolded) = current_match.take() {
        if let Some((property, value)) = unfolded.split_once(':') {
            let name = property.split(';').next().unwrap_or(property);
            if name.eq_ignore_ascii_case("CATEGORIES") {
                values.extend(category_names_from_ical_value(value));
            }
        }
    }
    values
}

pub fn category_names_from_ical_value(value: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            current.push('\\');
            current.push(character);
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == ',' {
            let category = unescape_ical_text(&current).trim().to_string();
            if !category.is_empty() {
                values.push(category);
            }
            current.clear();
        } else {
            current.push(character);
        }
    }
    if escaped {
        current.push('\\');
    }
    let category = unescape_ical_text(&current).trim().to_string();
    if !category.is_empty() {
        values.push(category);
    }
    values
}

fn unescape_ical_text(value: &str) -> String {
    if !value.contains('\\') {
        return value.to_string();
    }
    value
        .replace("\\n", "\n")
        .replace("\\N", "\n")
        .replace("\\,", ",")
        .replace("\\;", ";")
        .replace("\\\\", "\\")
}

fn normalize_category(mut category: CalendarCategory) -> io::Result<CalendarCategory> {
    category.id = category.id.trim().to_string();
    category.name = category.name.trim().to_string();
    category.color = category.color.trim().to_ascii_uppercase();
    if category.id.is_empty() || category.name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "A calendar category requires an ID and name",
        ));
    }
    if category.name.chars().count() > 100 || !valid_color(&category.color) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Calendar category or color is invalid",
        ));
    }
    Ok(category)
}

fn valid_color(value: &str) -> bool {
    matches!(value.len(), 7 | 9)
        && value.starts_with('#')
        && value[1..]
            .chars()
            .all(|character| character.is_ascii_hexdigit())
}

fn sort_categories(categories: &mut [CalendarCategory]) {
    categories.sort_by_key(|category| category.name.to_ascii_lowercase());
}

fn slug(value: &str) -> String {
    let slug = value
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character.to_ascii_lowercase())
            } else if character == '-' || character == '_' {
                Some('-')
            } else {
                None
            }
        })
        .take(32)
        .collect::<String>();
    if slug.is_empty() {
        "custom".to_string()
    } else {
        slug
    }
}

fn default_color(index: usize) -> String {
    [
        "#729FCF", "#AD7FA8", "#EF2929", "#F57900", "#8AE234", "#34E2E2",
    ][index % 6]
        .to_string()
}

fn default_categories() -> Vec<CalendarCategory> {
    vec![
        CalendarCategory {
            id: "category-work".to_string(),
            name: "Work".to_string(),
            color: "#729FCF".to_string(),
        },
        CalendarCategory {
            id: "category-personal".to_string(),
            name: "Personal".to_string(),
            color: "#AD7FA8".to_string(),
        },
        CalendarCategory {
            id: "category-birthday".to_string(),
            name: "Birthday".to_string(),
            color: "#EF2929".to_string(),
        },
        CalendarCategory {
            id: "category-travel".to_string(),
            name: "Travel".to_string(),
            color: "#F57900".to_string(),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_available_without_creating_a_file() {
        let root = temporary_directory("category-defaults");
        let path = root.join("categories.json");
        let store = CalendarCategoryStore::new(&path);
        assert_eq!(store.load().len(), 4);
        assert_eq!(store.color_for("work").as_deref(), Some("#729FCF"));
        assert!(!path.exists());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn categories_roundtrip_validate_and_delete() {
        let root = temporary_directory("category-roundtrip");
        let store = CalendarCategoryStore::new(root.join("categories.json"));
        let category = CalendarCategory {
            id: "category-custom".to_string(),
            name: "Projekt".to_string(),
            color: "#123abc".to_string(),
        };
        store
            .upsert(category.clone())
            .expect("category should save");
        assert_eq!(store.color_for("PROJEKT").as_deref(), Some("#123ABC"));
        assert_eq!(
            CalendarCategoryStore::new(store.path()).color_for("Projekt"),
            Some("#123ABC".to_string())
        );
        assert!(store.delete(&category.id).expect("category should delete"));
        assert!(store.color_for("Projekt").is_none());
        assert!(
            store
                .upsert(CalendarCategory {
                    id: "invalid".to_string(),
                    name: "Ungültig".to_string(),
                    color: "red".to_string(),
                })
                .is_err()
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn ensure_names_adds_only_new_categories() {
        let root = temporary_directory("category-ensure");
        let store = CalendarCategoryStore::new(root.join("categories.json"));
        assert_eq!(store.ensure_names(["Work", "Kunde", "Kunde"]).unwrap(), 1);
        assert_eq!(store.ensure_names(["Kunde"]).unwrap(), 0);
        assert!(store.color_for("Kunde").is_some());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn component_category_parser_preserves_escaped_commas() {
        let component = concat!(
            "BEGIN:VEVENT\r\n",
            "CATEGORIES:Arbeit,Privat\\, Familie\r\n",
            "CATEGORIES:Reise\r\n",
            "END:VEVENT\r\n"
        );
        assert_eq!(
            category_names_from_ical_component(component),
            vec![
                "Arbeit".to_string(),
                "Privat, Familie".to_string(),
                "Reise".to_string()
            ]
        );
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("meridian-{label}-{}", uuid::Uuid::new_v4()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temporary directory should be created");
        path
    }
}
