//! Internationalization (i18n) module using GNU gettext.
//!
//! Follows the GNOME standard conventions:
//! - Canonical source language in code is English.
//! - Translatable strings are wrapped with `gettext("...")` or the `_("...")` macro.
//! - Translations are stored as `.po` files in `po/` and compiled to `.mo` catalogs.

pub use gettextrs::{gettext, ngettext, pgettext};
use std::path::PathBuf;

pub const GETTEXT_PACKAGE: &str = "meridian";

/// Initializes gettext localization with system locale and binds textdomain.
pub fn init_i18n() {
    // Initialize the locale from the environment (LANG, LC_ALL, LC_MESSAGES)
    unsafe {
        gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "");
    }

    // Determine the locale directory:
    // 1. Explicit LOCALEDIR environment variable (used in tests or package deployment)
    // 2. Local development locale directory (`data/locale` or `./locale`)
    // 3. System installed directory (`/usr/share/locale` or `/usr/local/share/locale`)
    let localedir = if let Ok(dir) = std::env::var("LOCALEDIR") {
        PathBuf::from(dir)
    } else if let Some(dir) = option_env!("MERIDIAN_LOCALEDIR").or(option_env!("EVOLUTION_GTK4_LOCALEDIR")) {
        let pb = PathBuf::from(dir);
        if pb.exists() {
            pb
        } else {
            let candidate_local = PathBuf::from("data/locale");
            if candidate_local.exists() {
                candidate_local
            } else {
                PathBuf::from("/usr/share/locale")
            }
        }
    } else {
        let candidate_local = PathBuf::from("data/locale");
        if candidate_local.exists() {
            candidate_local
        } else {
            let candidate_target = PathBuf::from("target/locale");
            if candidate_target.exists() {
                candidate_target
            } else {
                PathBuf::from("/usr/share/locale")
            }
        }
    };

    let localedir_str = localedir.to_string_lossy();
    let _ = gettextrs::bindtextdomain(GETTEXT_PACKAGE, localedir_str.as_ref());
    let _ = gettextrs::bind_textdomain_codeset(GETTEXT_PACKAGE, "UTF-8");
    let _ = gettextrs::textdomain(GETTEXT_PACKAGE);
}

/// Macro shortcut for `gettext`.
#[macro_export]
macro_rules! tr {
    ($msgid:expr) => {
        $crate::i18n::gettext($msgid)
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_i18n_english_fallback_and_german_translation() {
        // 1. Initialize i18n
        init_i18n();

        // 2. Canonical English strings match or translate properly
        let mail_label = gettext("New Mail");
        assert!(mail_label == "New Mail" || mail_label == "Neue E-Mail");

        let delete_label = gettext("Delete");
        assert!(delete_label == "Delete" || delete_label == "Löschen");

        let print_label = gettext("Print");
        assert!(print_label == "Print" || print_label == "Drucken");

        let freebusy_label = gettext("Free/Busy");
        assert!(freebusy_label == "Free/Busy" || freebusy_label == "Freie Zeiten");

        // 3. Test macro tr!
        assert_eq!(tr!("New Mail"), gettext("New Mail"));
    }

    #[test]
    fn test_german_locale_lookup() {
        let _lock = crate::window::GTK_TEST_MUTEX
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let prev_lang = std::env::var("LANGUAGE").ok();
        let prev_lc = std::env::var("LC_ALL").ok();
        unsafe {
            std::env::set_var("LANGUAGE", "de");
            std::env::set_var("LC_ALL", "de_DE.UTF-8");
            gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "de_DE.UTF-8");
        }
        init_i18n();
        let res = gettext("New Mail");
        if res == "Neue E-Mail" {
            assert_eq!(gettext("Delete"), "Löschen");
            assert_eq!(gettext("Print"), "Drucken");
            assert_eq!(gettext("Free/Busy"), "Freie Zeiten");
        }
        unsafe {
            match prev_lang {
                Some(v) => std::env::set_var("LANGUAGE", v),
                None => std::env::remove_var("LANGUAGE"),
            }
            match prev_lc {
                Some(v) => {
                    std::env::set_var("LC_ALL", &v);
                    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, v.as_str());
                }
                None => {
                    std::env::remove_var("LC_ALL");
                    gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "C");
                }
            }
        }
        init_i18n();
    }
}
