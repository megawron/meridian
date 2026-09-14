// SPDX-License-Identifier: GPL-3.0-or-later
//
// GTK-free contracts for the memo surface.
//
// This module owns selection identity, action truth tables, portable
// serialization and preview preparation.  It intentionally does not mutate
// EDS or the local CalendarStore.  The view may consume the returned plans and
// perform confirmed backend operations, then rebuild from the authoritative
// result so a failed operation cannot create a phantom row.

use super::calendar_store_service::{MemoEditorDraft, MemoRecord};
use std::collections::{HashMap, HashSet};

const VCALENDAR_HEADER: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Evolution GTK4//EN\r\n";

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct MemoIdentity {
    pub source_uid: String,
    pub uid: String,
}

impl MemoIdentity {
    pub fn new(source_uid: impl Into<String>, uid: impl Into<String>) -> Self {
        Self {
            source_uid: source_uid.into(),
            uid: uid.into(),
        }
    }

    pub fn try_new(source_uid: impl Into<String>, uid: impl Into<String>) -> Result<Self, String> {
        let identity = Self::new(source_uid, uid);
        if identity.uid.trim().is_empty() {
            Err("Eine Memo-Identität benötigt eine UID".to_string())
        } else {
            Ok(identity)
        }
    }
}

impl From<&MemoRecord> for MemoIdentity {
    fn from(record: &MemoRecord) -> Self {
        Self::new(record.source_uid.clone(), record.uid.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoSelection {
    selected: Vec<MemoIdentity>,
    focus: Option<MemoIdentity>,
    generation: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectionRestore {
    pub generation: u64,
    pub selected: Vec<MemoIdentity>,
    pub focus: Option<MemoIdentity>,
}

impl MemoSelection {
    pub fn selected(&self) -> &[MemoIdentity] {
        &self.selected
    }

    pub fn focus(&self) -> Option<&MemoIdentity> {
        self.focus.as_ref()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn len(&self) -> usize {
        self.selected.len()
    }

    pub fn is_empty(&self) -> bool {
        self.selected.is_empty()
    }

    pub fn contains(&self, identity: &MemoIdentity) -> bool {
        self.selected.iter().any(|selected| selected == identity)
    }

    pub fn set_generation(&mut self, generation: u64) {
        self.generation = generation;
    }

    pub fn select(&mut self, identity: MemoIdentity) {
        if !self.contains(&identity) {
            self.selected.push(identity.clone());
        }
        self.focus = Some(identity);
    }

    pub fn set_selected(&mut self, identities: impl IntoIterator<Item = MemoIdentity>) {
        self.selected.clear();
        for identity in identities {
            if !self.contains(&identity) {
                self.selected.push(identity);
            }
        }
        self.focus = self.selected.last().cloned();
    }

    pub fn toggle(&mut self, identity: MemoIdentity) {
        if let Some(index) = self
            .selected
            .iter()
            .position(|selected| selected == &identity)
        {
            self.selected.remove(index);
            if self.focus.as_ref() == Some(&identity) {
                self.focus = self.selected.last().cloned();
            }
        } else {
            self.selected.push(identity.clone());
            self.focus = Some(identity);
        }
    }

    pub fn set_focus(&mut self, identity: Option<MemoIdentity>) {
        self.focus = identity.filter(|identity| self.contains(identity));
    }

    pub fn clear(&mut self) {
        self.selected.clear();
        self.focus = None;
    }

    pub fn select_all_visible<'a, I>(&mut self, identities: I)
    where
        I: IntoIterator<Item = &'a MemoIdentity>,
    {
        self.selected.clear();
        let mut seen = HashSet::new();
        self.selected.extend(
            identities
                .into_iter()
                .filter(|identity| seen.insert((*identity).clone()))
                .cloned(),
        );
        self.focus = self.selected.last().cloned();
    }

    /// Restore selection after a refresh using the complete `(source_uid,uid)`
    /// identity.  Missing records are dropped, never recreated as phantom
    /// rows.  The focus falls back to the last surviving selected row.
    pub fn restore_after_refresh(
        &mut self,
        records: &[MemoRecord],
        generation: u64,
    ) -> SelectionRestore {
        let available = records
            .iter()
            .map(MemoIdentity::from)
            .collect::<HashSet<_>>();
        self.selected
            .retain(|identity| available.contains(identity));
        self.focus = self
            .focus
            .take()
            .filter(|identity| available.contains(identity))
            .or_else(|| self.selected.last().cloned());
        self.generation = generation;
        SelectionRestore {
            generation,
            selected: self.selected.clone(),
            focus: self.focus.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoActionState {
    pub selected_count: usize,
    pub has_selection: bool,
    pub single_selection: bool,
    pub multiple_selection: bool,
    pub all_selected_sources_writable: bool,
    pub mixed_read_only_selection: bool,
    pub selection_has_url: bool,
    pub selection_has_valid_url: bool,
    pub can_open: bool,
    pub can_forward: bool,
    pub can_open_url: bool,
    pub can_print: bool,
    pub can_save_as: bool,
    pub can_export: bool,
    pub can_copy: bool,
    pub can_cut: bool,
    pub can_delete: bool,
    pub can_select_all: bool,
}

impl MemoActionState {
    pub fn from_selection<F>(records: &[MemoRecord], is_writable: F) -> Self
    where
        F: Fn(&str) -> bool,
    {
        let selected_count = records.len();
        let single_selection = selected_count == 1;
        let multiple_selection = selected_count > 1;
        let all_selected_sources_writable =
            selected_count > 0 && records.iter().all(|record| is_writable(&record.source_uid));
        let mixed_read_only_selection = selected_count > 0
            && records
                .iter()
                .any(|record| !is_writable(&record.source_uid))
            && records.iter().any(|record| is_writable(&record.source_uid));
        let urls = records
            .iter()
            .filter_map(|record| MemoEditorDraft::parse(&record.component).ok())
            .filter_map(|draft| draft.url)
            .collect::<Vec<_>>();
        let selection_has_url = !urls.is_empty();
        let selection_has_valid_url = urls.iter().any(|url| validate_open_url(url).is_ok());

        Self {
            selected_count,
            has_selection: selected_count > 0,
            single_selection,
            multiple_selection,
            all_selected_sources_writable,
            mixed_read_only_selection,
            selection_has_url,
            selection_has_valid_url,
            can_open: single_selection,
            can_forward: single_selection,
            can_open_url: single_selection && selection_has_valid_url,
            can_print: single_selection,
            can_save_as: single_selection,
            can_export: selected_count > 0,
            can_copy: selected_count > 0,
            can_cut: all_selected_sources_writable,
            can_delete: all_selected_sources_writable,
            // The caller may have rows that are not selected; selection
            // count alone cannot decide whether "Alle auswählen" is useful.
            can_select_all: true,
        }
    }

    pub fn for_writable_sources(
        records: &[MemoRecord],
        writable_sources: &HashSet<String>,
    ) -> Self {
        Self::from_selection(records, |source_uid| writable_sources.contains(source_uid))
    }

    pub fn can_paste(&self, destination_writable: bool, clipboard_has_memo: bool) -> bool {
        destination_writable && clipboard_has_memo
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedMemoComponent {
    pub uid: String,
    pub component: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoMutationFailure {
    pub identity: MemoIdentity,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MemoBatchResult {
    pub succeeded: Vec<MemoIdentity>,
    pub failed: Vec<MemoMutationFailure>,
}

impl MemoBatchResult {
    pub fn is_complete_success(&self) -> bool {
        self.failed.is_empty()
    }

    pub fn is_empty(&self) -> bool {
        self.succeeded.is_empty() && self.failed.is_empty()
    }

    pub fn visible_error_text(&self) -> String {
        self.failed
            .iter()
            .map(|failure| {
                format!(
                    "{} / {}: {}",
                    failure.identity.source_uid, failure.identity.uid, failure.error
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoPasteItem {
    pub identity: MemoIdentity,
    pub component: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoPreviewAttachment {
    pub label: String,
    pub value: String,
    pub inline: bool,
    pub inert: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoPreview {
    pub summary: String,
    pub description: String,
    pub categories: Vec<String>,
    pub url: Option<String>,
    pub source_uid: String,
    pub source_display_name: String,
    pub modified: String,
    pub attachments: Vec<MemoPreviewAttachment>,
    /// Pango-safe, read-only markup for the GTK preview.  No attachment is
    /// loaded, decoded, linked or otherwise executed by this value.
    pub markup: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MemoPreviewOrientation {
    #[default]
    Classic,
    Vertical,
}

impl MemoPreviewOrientation {
    pub fn setting_value(self) -> &'static str {
        match self {
            Self::Classic => "classic",
            Self::Vertical => "vertical",
        }
    }

    pub fn parse_setting(value: &str) -> Result<Self, String> {
        match value.trim().to_ascii_lowercase().as_str() {
            "classic" => Ok(Self::Classic),
            "vertical" => Ok(Self::Vertical),
            _ => Err(
                "Ungültige Memo-Vorschauausrichtung; erwartet classic oder vertical".to_string(),
            ),
        }
    }

    pub fn from_setting(value: &str) -> Self {
        Self::parse_setting(value).unwrap_or_default()
    }

    pub fn index(self) -> u32 {
        match self {
            Self::Classic => 0,
            Self::Vertical => 1,
        }
    }

    pub fn from_index(index: u32) -> Self {
        match index {
            1 => Self::Vertical,
            _ => Self::Classic,
        }
    }

    pub fn default_position(self) -> i32 {
        match self {
            Self::Classic => 360,
            Self::Vertical => 360,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoPreviewState {
    pub visible: bool,
    pub orientation: MemoPreviewOrientation,
}

impl Default for MemoPreviewState {
    fn default() -> Self {
        Self {
            visible: true,
            orientation: MemoPreviewOrientation::Classic,
        }
    }
}

impl MemoPreviewState {
    pub fn toggle(&mut self) {
        self.visible = !self.visible;
    }

    pub fn set_visible(&mut self, visible: bool) {
        self.visible = visible;
    }

    pub fn set_orientation(&mut self, orientation: MemoPreviewOrientation) {
        self.orientation = orientation;
    }
}

pub fn validate_open_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("Die Memo-URL ist leer".to_string());
    }
    if value
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err("Die Memo-URL enthält Leer- oder Steuerzeichen".to_string());
    }
    if value.contains('"')
        || value.contains('\'')
        || value.contains('<')
        || value.contains('>')
        || value.contains('\\')
    {
        return Err("Die Memo-URL enthält unsichere Zeichen".to_string());
    }
    let Some(colon) = value.find(':') else {
        return Err("Die Memo-URL benötigt ein unterstütztes URI-Schema".to_string());
    };
    let scheme = &value[..colon];
    if scheme.is_empty()
        || !scheme.chars().enumerate().all(|(index, character)| {
            if index == 0 {
                character.is_ascii_alphabetic()
            } else {
                character.is_ascii_alphanumeric() || matches!(character, '+' | '-' | '.')
            }
        })
    {
        return Err("Ungültiges URI-Schema in der Memo-URL".to_string());
    }
    let scheme = scheme.to_ascii_lowercase();
    let remainder = value.get(colon + 1..).unwrap_or_default();
    if scheme == "http" || scheme == "https" {
        if !remainder.starts_with("//") || remainder.len() <= 2 {
            return Err("HTTP(S)-Memo-URLs benötigen einen Host".to_string());
        }
        let authority_end = remainder
            .get(2..)
            .and_then(|remainder| remainder.find(['/', '?', '#']))
            .map_or(remainder.len(), |end| end + 2);
        let authority = &remainder[2..authority_end];
        let authority = canonical_http_authority(authority)?;
        return Ok(format!(
            "{scheme}://{authority}{}",
            &remainder[authority_end..]
        ));
    } else if scheme == "mailto" {
        if remainder.is_empty() || remainder.starts_with("//") {
            return Err("Eine mailto-Memo-URL benötigt ein Ziel".to_string());
        }
        return Ok(format!("mailto:{remainder}"));
    } else {
        return Err("Nur http, https und mailto sind für Open URL portabel erlaubt".to_string());
    }
}

fn canonical_http_authority(authority: &str) -> Result<String, String> {
    if authority.is_empty()
        || authority.contains('@')
        || authority.contains('\\')
        || authority.contains('"')
        || authority.contains('\'')
        || authority.contains('<')
        || authority.contains('>')
    {
        return Err("Die Memo-URL enthält keinen gültigen HTTP-Host".to_string());
    }

    if authority.starts_with('[') {
        let Some(end) = authority.find(']') else {
            return Err("Die Memo-URL enthält keinen gültigen IPv6-Host".to_string());
        };
        let host = &authority[1..end];
        let host = host
            .parse::<std::net::Ipv6Addr>()
            .map_err(|_| "Die Memo-URL enthält keinen gültigen IPv6-Host".to_string())?;
        let suffix = &authority[end + 1..];
        let port = canonical_http_port_suffix(suffix)?;
        return Ok(format!("[{host}]{port}"));
    }

    if authority.matches(':').count() > 1 {
        return Err("IPv6-Memo-Hosts müssen in eckigen Klammern stehen".to_string());
    }
    let (host, port) = authority
        .split_once(':')
        .map_or((authority, None), |(host, port)| (host, Some(port)));
    if host.is_empty() || host == "." || host == ".." {
        return Err("Die Memo-URL enthält keinen gültigen HTTP-Host".to_string());
    }
    if !host
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
    {
        return Err("Die Memo-URL enthält keinen gültigen HTTP-Host".to_string());
    }
    let port = port.map(|port| canonical_http_port_suffix(&format!(":{port}")));
    let port = port.transpose()?.unwrap_or_default();
    Ok(format!("{}{port}", host.to_ascii_lowercase()))
}

fn canonical_http_port_suffix(suffix: &str) -> Result<String, String> {
    if suffix.is_empty() {
        return Ok(String::new());
    }
    let Some(port) = suffix.strip_prefix(':') else {
        return Err("Die Memo-URL enthält einen ungültigen Port".to_string());
    };
    if port.is_empty() || !port.chars().all(|character| character.is_ascii_digit()) {
        return Err("Die Memo-URL enthält einen ungültigen Port".to_string());
    }
    port.parse::<u16>()
        .map(|port| format!(":{port}"))
        .map_err(|_| "Die Memo-URL enthält einen ungültigen Port".to_string())
}

pub fn validated_url_for_record(record: &MemoRecord) -> Result<String, String> {
    let draft = MemoEditorDraft::parse(&record.component)
        .map_err(|error| format!("Memo-Komponente ist ungültig: {error}"))?;
    let url = draft
        .url
        .ok_or_else(|| "Die ausgewählte Memo hat keine URL".to_string())?;
    validate_open_url(&url)
}

pub fn validate_memo_component(raw: &str) -> Result<ValidatedMemoComponent, String> {
    let mut components = parse_memo_components(raw)?;
    if components.len() != 1 {
        return Err("Erwartet wird genau ein VJOURNAL".to_string());
    }
    Ok(components.remove(0))
}

pub fn validate_memo_record(record: &MemoRecord) -> Result<ValidatedMemoComponent, String> {
    let component = validate_memo_component(&record.component)?;
    if record.uid.trim().is_empty() {
        return Err("Die Memo-Zeile hat keine UID".to_string());
    }
    if component.uid != record.uid {
        return Err(format!(
            "UID-Mismatch: Zeile {} enthält eine Komponente mit UID {}",
            record.uid, component.uid
        ));
    }
    Ok(component)
}

/// Build the portable iTIP-like payload.  Real iTIP forwarding remains a
/// Linux handoff; this value is deterministic and never changes the source.
pub fn build_forward_publish_payload(record: &MemoRecord) -> Result<String, String> {
    let component = validate_memo_record(record)?;
    Ok(envelope_from_components(
        &[component.component],
        Some("PUBLISH"),
    ))
}

pub fn forward_publish_payload(component: &str) -> Result<String, String> {
    let component = validate_memo_component(component)?;
    Ok(envelope_from_components(
        &[component.component],
        Some("PUBLISH"),
    ))
}

pub fn build_clipboard_payload(
    records: &[MemoRecord],
    selection: &[MemoIdentity],
) -> Result<String, String> {
    let selected = selected_records(records, selection)?;
    let components = selected
        .iter()
        .map(|record| validate_memo_record(record))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(envelope_from_components(
        &components
            .into_iter()
            .map(|component| component.component)
            .collect::<Vec<_>>(),
        None,
    ))
}

/// Deterministic multi-export.  Component property order, folded lines,
/// nested VALARM blocks, ATTACH properties and unknown properties are copied
/// byte-for-byte apart from the required CRLF normalization and one envelope.
pub fn build_multi_export(
    records: &[MemoRecord],
    selection: &[MemoIdentity],
) -> Result<String, String> {
    let selected = selected_records(records, selection)?;
    let mut components = selected
        .iter()
        .map(|record| {
            validate_memo_record(record)
                .map(|component| (MemoIdentity::from(*record), component.component))
        })
        .collect::<Result<Vec<_>, _>>()?;
    components.sort_by(|left, right| left.0.cmp(&right.0));
    if components
        .windows(2)
        .any(|window| window[0].0 == window[1].0)
    {
        return Err("Die Auswahl enthält dieselbe (source_uid,uid)-Identität mehrfach".to_string());
    }
    Ok(envelope_from_components(
        &components
            .into_iter()
            .map(|(_, component)| component)
            .collect::<Vec<_>>(),
        None,
    ))
}

pub fn build_portable_print_document(record: &MemoRecord) -> Result<String, String> {
    Ok(build_print_document(record)?.html)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoPrintDocument {
    pub html: String,
    pub plain_text: String,
}

pub fn build_print_document(record: &MemoRecord) -> Result<MemoPrintDocument, String> {
    let component = validate_memo_record(record)?;
    let draft = MemoEditorDraft::parse(&component.component)
        .map_err(|error| format!("Memo-Komponente ist ungültig: {error}"))?;
    let attachments = draft
        .attachments
        .iter()
        .map(preview_attachment)
        .collect::<Vec<_>>();
    let description = escape_html(&draft.description).replace('\n', "<br>");
    let categories = draft
        .categories
        .iter()
        .map(|category| escape_html(category))
        .collect::<Vec<_>>()
        .join(", ");
    let url = draft.url.as_deref().unwrap_or_default();
    let attachment_bar = attachments
        .iter()
        .map(|attachment| {
            format!(
                "<li>{}: {}</li>",
                escape_html(&attachment.label),
                escape_html(&attachment.value)
            )
        })
        .collect::<Vec<_>>()
        .join("");
    let html = format!(
        "<article><h1>Zusammenfassung: {}</h1><dl><dt>Beschreibung</dt><dd>{}</dd><dt>Kategorien</dt><dd>{}</dd><dt>URL</dt><dd>{}</dd><dt>Quelle</dt><dd>{}</dd><dt>Geändert</dt><dd>{}</dd></dl><h2>Anhänge</h2><ul>{}</ul></article>",
        escape_html(&draft.summary),
        description,
        categories,
        escape_html(url),
        escape_html(&record.source_display_name),
        escape_html(&record.updated),
        attachment_bar
    );
    let plain_text = format!(
        "{}\n\n{}\nKategorien: {}\nURL: {}\nQuelle: {}\nGeändert: {}\nAnhänge: {}",
        draft.summary,
        draft.description,
        draft.categories.join(", "),
        url,
        record.source_display_name,
        record.updated,
        attachments
            .iter()
            .map(|attachment| format!("{} ({})", attachment.label, attachment.value))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(MemoPrintDocument { html, plain_text })
}

pub fn preview_for_selection(
    records: &[MemoRecord],
    selection: &[MemoIdentity],
) -> Result<Option<MemoPreview>, String> {
    if selection.len() != 1 {
        return Ok(None);
    }
    let selected = selected_records(records, selection)?;
    let record = selected
        .first()
        .ok_or_else(|| "Die Vorschauauswahl ist nicht mehr vorhanden".to_string())?;
    let component = validate_memo_record(record)?;
    let draft = MemoEditorDraft::parse(&component.component)
        .map_err(|error| format!("Memo-Komponente ist ungültig: {error}"))?;
    let attachments = draft
        .attachments
        .iter()
        .map(preview_attachment)
        .collect::<Vec<_>>();
    let attachment_markup = attachments
        .iter()
        .map(|attachment| {
            format!(
                "<span weight=\"bold\">{}</span>: {}",
                escape_markup(&attachment.label),
                escape_markup(&attachment.value)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let markup = format!(
        "<b>Zusammenfassung: {}</b>\nBeschreibung: {}\nKategorien: {}\nURL: {}\nQuelle: {}\nGeändert: {}\nAnhänge (inert):\n{}",
        escape_markup(&draft.summary),
        escape_markup(&draft.description),
        escape_markup(&draft.categories.join(", ")),
        escape_markup(draft.url.as_deref().unwrap_or_default()),
        escape_markup(&record.source_display_name),
        escape_markup(&record.updated),
        attachment_markup
    );
    Ok(Some(MemoPreview {
        summary: draft.summary,
        description: draft.description,
        categories: draft.categories,
        url: draft.url,
        source_uid: record.source_uid.clone(),
        source_display_name: record.source_display_name.clone(),
        modified: record.updated.clone(),
        attachments,
        markup,
    }))
}

pub fn plan_delete<F>(
    records: &[MemoRecord],
    selection: &[MemoIdentity],
    is_writable: F,
) -> Result<Vec<MemoRecord>, MemoBatchResult>
where
    F: Fn(&str) -> bool,
{
    let selected = match selected_records(records, selection) {
        Ok(selected) => selected,
        Err(error) => {
            return Err(MemoBatchResult {
                failed: selection
                    .iter()
                    .cloned()
                    .map(|identity| MemoMutationFailure {
                        identity,
                        error: error.clone(),
                    })
                    .collect(),
                ..MemoBatchResult::default()
            });
        }
    };
    let failures = selected
        .iter()
        .filter(|record| !is_writable(&record.source_uid))
        .map(|record| MemoMutationFailure {
            identity: MemoIdentity::from(*record),
            error: "Die Quelle ist schreibgeschützt".to_string(),
        })
        .collect::<Vec<_>>();
    if !failures.is_empty() {
        // A mixed/read-only selection is rejected before any backend call.
        return Err(MemoBatchResult {
            failed: failures,
            ..MemoBatchResult::default()
        });
    }
    Ok(selected.into_iter().cloned().collect())
}

pub fn plan_paste(
    payload: &str,
    destination_source_uid: &str,
    destination_writable: bool,
) -> Result<Vec<MemoPasteItem>, String> {
    if destination_source_uid.trim().is_empty() {
        return Err("Die Zielquelle für das Einfügen ist leer".to_string());
    }
    if !destination_writable {
        return Err("Die Zielquelle ist schreibgeschützt".to_string());
    }
    let components = parse_memo_components(payload)?;
    if components.is_empty() {
        return Err("Die Zwischenablage enthält kein VJOURNAL".to_string());
    }
    let mut seen = HashSet::new();
    components
        .into_iter()
        .map(|component| {
            if !seen.insert(component.uid.clone()) {
                return Err(format!(
                    "Doppelte UID in der Zwischenablage: {}",
                    component.uid
                ));
            }
            Ok(MemoPasteItem {
                identity: MemoIdentity::new(destination_source_uid, component.uid),
                component: component.component,
            })
        })
        .collect()
}

/// Preflight a paste against the current destination snapshot.  A duplicate
/// destination identity is rejected before any ECal create call, so a batch
/// paste cannot partially mutate a source merely because one UID already
/// exists there.
pub fn plan_paste_into(
    payload: &str,
    destination_source_uid: &str,
    destination_writable: bool,
    existing: &[MemoRecord],
) -> Result<Vec<MemoPasteItem>, String> {
    let plan = plan_paste(payload, destination_source_uid, destination_writable)?;
    let existing = existing
        .iter()
        .map(MemoIdentity::from)
        .collect::<HashSet<_>>();
    if let Some(item) = plan.iter().find(|item| existing.contains(&item.identity)) {
        return Err(format!(
            "Memo {} / {} ist in der Zielquelle bereits vorhanden",
            item.identity.source_uid, item.identity.uid
        ));
    }
    Ok(plan)
}

pub fn source_is_writable(source_uid: &str) -> bool {
    !source_uid.trim().is_empty()
}

fn selected_records<'a>(
    records: &'a [MemoRecord],
    selection: &[MemoIdentity],
) -> Result<Vec<&'a MemoRecord>, String> {
    let mut by_identity = HashMap::new();
    let mut duplicate_records = HashSet::new();
    for record in records {
        let identity = MemoIdentity::from(record);
        if by_identity.insert(identity.clone(), record).is_some() {
            duplicate_records.insert(identity);
        }
    }
    let mut selected = Vec::with_capacity(selection.len());
    let mut seen = HashSet::new();
    for identity in selection {
        if !seen.insert(identity) {
            return Err(format!(
                "Die Auswahl enthält die Identität {} / {} mehrfach",
                identity.source_uid, identity.uid
            ));
        }
        if duplicate_records.contains(identity) {
            return Err(format!(
                "Die Daten enthalten die Identität {} / {} mehrfach",
                identity.source_uid, identity.uid
            ));
        }
        let Some(record) = by_identity.get(identity).copied() else {
            return Err(format!(
                "Memo {} / {} ist nach dem Refresh nicht mehr vorhanden",
                identity.source_uid, identity.uid
            ));
        };
        selected.push(record);
    }
    Ok(selected)
}

fn parse_memo_components(raw: &str) -> Result<Vec<ValidatedMemoComponent>, String> {
    let lines = physical_lines(raw);
    if lines.is_empty() {
        return Err("Die iCalendar-Daten sind leer".to_string());
    }
    validate_structure(&lines)?;
    let first = lines.first().map(String::as_str).unwrap_or_default();
    let is_envelope = first.eq_ignore_ascii_case("BEGIN:VCALENDAR");
    if is_envelope
        && !lines
            .last()
            .is_some_and(|line| line.eq_ignore_ascii_case("END:VCALENDAR"))
    {
        return Err("VCALENDAR enthält kein abschließendes END:VCALENDAR".to_string());
    }
    if !is_envelope && !first.eq_ignore_ascii_case("BEGIN:VJOURNAL") {
        return Err("Die iCalendar-Daten sind kein VJOURNAL".to_string());
    }
    if is_envelope {
        validate_envelope_contents(&lines)?;
    }

    let mut components = Vec::new();
    let mut index = 0;
    let mut last_component_end = None;
    while index < lines.len() {
        if !lines[index].eq_ignore_ascii_case("BEGIN:VJOURNAL") {
            index += 1;
            continue;
        }
        let start = index;
        let mut depth = 1usize;
        index += 1;
        while index < lines.len() && depth > 0 {
            if lines[index].eq_ignore_ascii_case("BEGIN:VJOURNAL") {
                depth += 1;
            } else if lines[index].eq_ignore_ascii_case("END:VJOURNAL") {
                depth = depth.saturating_sub(1);
            }
            index += 1;
        }
        if depth != 0 {
            return Err("VJOURNAL enthält kein abschließendes END:VJOURNAL".to_string());
        }
        let end = index - 1;
        let component = lines[start..=end].join("\r\n") + "\r\n";
        let uid = top_level_uid(&component)
            .ok_or_else(|| "VJOURNAL enthält keine nichtleere UID".to_string())?;
        MemoEditorDraft::parse(&component)
            .map_err(|error| format!("Ungültiges VJOURNAL: {error}"))?;
        components.push(ValidatedMemoComponent { uid, component });
        last_component_end = Some(end);
    }

    if components.is_empty() {
        return Err("Die iCalendar-Daten enthalten kein VJOURNAL".to_string());
    }
    if !is_envelope && components.len() != 1 {
        return Err("Mehrere VJOURNALs benötigen eine VCALENDAR-Hülle".to_string());
    }
    if !is_envelope && last_component_end != Some(lines.len() - 1) {
        return Err("Ein einzelnes VJOURNAL darf keine zusätzlichen Daten enthalten".to_string());
    }
    Ok(components)
}

fn validate_envelope_contents(lines: &[String]) -> Result<(), String> {
    let mut stack = Vec::<String>::new();
    for line in lines {
        let upper = line.to_ascii_uppercase();
        if let Some(kind) = upper.strip_prefix("BEGIN:") {
            if kind == "VCALENDAR" && !stack.is_empty() {
                return Err("VCALENDAR darf nicht verschachtelt werden".to_string());
            }
            if stack.len() == 1 && kind != "VJOURNAL" {
                return Err(format!(
                    "VCALENDAR enthält keine Memo-Komponente: BEGIN:{kind}"
                ));
            }
            stack.push(kind.to_string());
            continue;
        }
        if let Some(kind) = upper.strip_prefix("END:") {
            let Some(open) = stack.pop() else {
                return Err(format!("Unerwartetes END:{kind}"));
            };
            if open != kind {
                return Err(format!("BEGIN:{open} und END:{kind} passen nicht zusammen"));
            }
            continue;
        }
        if stack.is_empty() {
            return Err("Daten außerhalb der VCALENDAR-Hülle sind nicht erlaubt".to_string());
        }
    }
    if !stack.is_empty() {
        return Err("VCALENDAR enthält eine nicht geschlossene Komponente".to_string());
    }
    Ok(())
}

fn validate_structure(lines: &[String]) -> Result<(), String> {
    let mut stack = Vec::<String>::new();
    let mut vjournal_depth = 0usize;
    for line in lines {
        let upper = line.to_ascii_uppercase();
        if let Some(kind) = upper.strip_prefix("BEGIN:") {
            if kind == "VJOURNAL" {
                if vjournal_depth != 0 {
                    return Err(
                        "VJOURNAL darf keine weitere VJOURNAL-Komponente enthalten".to_string()
                    );
                }
                vjournal_depth += 1;
            }
            stack.push(kind.to_string());
        } else if let Some(kind) = upper.strip_prefix("END:") {
            let kind = kind.to_string();
            let Some(open) = stack.pop() else {
                return Err(format!("Unerwartetes END:{kind}"));
            };
            if open != kind {
                return Err(format!("BEGIN:{open} und END:{kind} passen nicht zusammen"));
            }
            if kind == "VJOURNAL" {
                vjournal_depth = vjournal_depth.saturating_sub(1);
            }
        }
    }
    if let Some(open) = stack.last() {
        return Err(format!("BEGIN:{open} bleibt offen"));
    }
    if lines.iter().any(|line| {
        let upper = line.to_ascii_uppercase();
        upper == "BEGIN:VEVENT"
            || upper == "BEGIN:VTODO"
            || upper == "END:VEVENT"
            || upper == "END:VTODO"
    }) {
        return Err("Nur VJOURNAL-Komponenten sind für die Memo-Oberfläche erlaubt".to_string());
    }
    Ok(())
}

fn top_level_uid(component: &str) -> Option<String> {
    let mut depth = 0usize;
    for line in unfold_lines(component) {
        let upper = line.to_ascii_uppercase();
        if upper == "BEGIN:VJOURNAL" {
            depth += 1;
            continue;
        }
        if upper.starts_with("BEGIN:") {
            depth += 1;
            continue;
        }
        if upper.starts_with("END:") {
            depth = depth.saturating_sub(1);
            continue;
        }
        if depth != 1 {
            continue;
        }
        let Some((property, value)) = line.split_once(':') else {
            continue;
        };
        if property
            .split(';')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case("UID"))
        {
            let value = unescape_ical_text(value);
            if !value.trim().is_empty() && !value.chars().any(char::is_control) {
                return Some(value);
            }
        }
    }
    None
}

fn physical_lines(raw: &str) -> Vec<String> {
    raw.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(ToOwned::to_owned)
        .filter(|line| !line.is_empty())
        .collect()
}

fn unfold_lines(raw: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for line in physical_lines(raw) {
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(previous) = lines.last_mut() {
                previous.push_str(&line[1..]);
            }
        } else {
            lines.push(line);
        }
    }
    lines
}

fn envelope_from_components(components: &[String], method: Option<&str>) -> String {
    let mut output = String::from(VCALENDAR_HEADER);
    if let Some(method) = method {
        output.push_str("METHOD:");
        output.push_str(method);
        output.push_str("\r\n");
    }
    for component in components {
        let lines = physical_lines(component);
        output.push_str(&lines.join("\r\n"));
        output.push_str("\r\n");
    }
    output.push_str("END:VCALENDAR\r\n");
    output
}

fn preview_attachment(
    attachment: &super::calendar_store_service::MemoAttachment,
) -> MemoPreviewAttachment {
    if attachment.is_inline() {
        MemoPreviewAttachment {
            label: attachment
                .filename
                .clone()
                .unwrap_or_else(|| "Inline-Anhang".to_string()),
            value: attachment
                .inline_content
                .clone()
                .unwrap_or_else(|| "(Inline-Inhalt nicht geöffnet)".to_string()),
            inline: true,
            inert: true,
        }
    } else {
        MemoPreviewAttachment {
            label: attachment
                .filename
                .clone()
                .unwrap_or_else(|| "URI-Anhang".to_string()),
            value: attachment.uri.clone().unwrap_or_default(),
            inline: false,
            inert: true,
        }
    }
}

fn unescape_ical_text(value: &str) -> String {
    let mut output = String::new();
    let mut escaped = false;
    for character in value.chars() {
        if escaped {
            match character {
                'n' | 'N' => output.push('\n'),
                ',' => output.push(','),
                ';' => output.push(';'),
                '\\' => output.push('\\'),
                other => {
                    output.push('\\');
                    output.push(other);
                }
            }
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else {
            output.push(character);
        }
    }
    if escaped {
        output.push('\\');
    }
    output
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn escape_markup(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn component(uid: &str, summary: &str, url: Option<&str>) -> String {
        let url = url.map(|url| format!("URL:{url}\r\n")).unwrap_or_default();
        format!(
            "BEGIN:VJOURNAL\r\nUID:{uid}\r\nSUMMARY:{summary}\r\n{url}DESCRIPTION:body\r\nATTACH;FMTTYPE=text/plain:URI:https://example.test/a\r\nBEGIN:VALARM\r\nACTION:DISPLAY\r\nDESCRIPTION:alarm\r\nEND:VALARM\r\nX-ORDER:kept\r\nEND:VJOURNAL\r\n"
        )
    }

    fn record(source_uid: &str, uid: &str, summary: &str, url: Option<&str>) -> MemoRecord {
        MemoRecord {
            uid: uid.to_string(),
            source_uid: source_uid.to_string(),
            source_display_name: format!("Source {source_uid}"),
            component: component(uid, summary, url),
            summary: summary.to_string(),
            description: "body".to_string(),
            updated: "2026-08-23 10:00".to_string(),
        }
    }

    #[test]
    fn selection_uses_source_and_uid_and_restores_without_phantoms() {
        let first = record("source-a", "same", "A", None);
        let second = record("source-b", "same", "B", None);
        let mut selection = MemoSelection::default();
        selection.select(MemoIdentity::from(&first));
        selection.select(MemoIdentity::from(&second));
        assert_eq!(selection.len(), 2);
        let restored = selection.restore_after_refresh(&[second.clone()], 7);
        assert_eq!(restored.selected, vec![MemoIdentity::from(&second)]);
        assert_eq!(restored.focus, Some(MemoIdentity::from(&second)));
        assert_eq!(selection.generation(), 7);
    }

    #[test]
    fn action_truth_table_distinguishes_zero_one_many_and_read_only() {
        let zero = MemoActionState::from_selection(&[], |_| true);
        assert!(!zero.can_open && !zero.can_copy && !zero.can_delete);
        assert!(zero.can_select_all);

        let one = record("source-a", "one", "One", Some("https://example.test"));
        let one_state = MemoActionState::from_selection(std::slice::from_ref(&one), |_| true);
        assert!(one_state.can_open);
        assert!(one_state.can_forward);
        assert!(one_state.can_open_url);
        assert!(one_state.can_print && one_state.can_save_as);

        let many = vec![
            record("source-a", "a", "A", None),
            record("source-a", "b", "B", None),
        ];
        let many_state = MemoActionState::from_selection(&many, |_| true);
        assert!(!many_state.can_open && !many_state.can_forward);
        assert!(many_state.can_export && many_state.can_copy && many_state.can_delete);

        let mixed = MemoActionState::from_selection(&many, |source| source == "source-a");
        assert!(!mixed.mixed_read_only_selection);
        let read_only = vec![record("", "read-only", "R", None), many[0].clone()];
        let mixed = MemoActionState::from_selection(&read_only, |source| source == "source-a");
        assert!(mixed.mixed_read_only_selection);
        assert!(!mixed.can_cut && !mixed.can_delete);
    }

    #[test]
    fn urls_are_validated_before_open_handoff() {
        assert_eq!(
            validate_open_url("https://example.test/a").unwrap(),
            "https://example.test/a"
        );
        assert_eq!(
            validate_open_url("HTTPS://Example.TEST:443/a").unwrap(),
            "https://example.test:443/a"
        );
        assert_eq!(
            validate_open_url("mailto:person@example.test").unwrap(),
            "mailto:person@example.test"
        );
        assert_eq!(
            validate_open_url("HTTPS://[2001:0DB8:0:0:0:0:0:1]:00443/a").unwrap(),
            "https://[2001:db8::1]:443/a"
        );
        for invalid in [
            "",
            "javascript:alert(1)",
            "file:///tmp/a",
            "https://",
            "https://example.test:bad/a",
            "https://user:pass@example.test/a",
            "https://example.test/ bad",
            "https://example.test\nX",
            "mailto://person@example.test",
        ] {
            assert!(validate_open_url(invalid).is_err(), "{invalid:?}");
        }
    }

    #[test]
    fn publish_requires_one_matching_vjournal_and_leaves_original_unchanged() {
        let original = record("source-a", "uid", "Title", None);
        let payload = build_forward_publish_payload(&original).expect("publish payload");
        assert!(payload.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(payload.contains("METHOD:PUBLISH\r\n"));
        assert_eq!(payload.matches("BEGIN:VJOURNAL").count(), 1);
        assert_eq!(payload.matches("END:VJOURNAL").count(), 1);
        assert_eq!(original.component, component("uid", "Title", None));

        let multiple = concat!(
            "BEGIN:VCALENDAR\r\n",
            "BEGIN:VJOURNAL\r\nUID:one\r\nEND:VJOURNAL\r\n",
            "BEGIN:VJOURNAL\r\nUID:two\r\nEND:VJOURNAL\r\n",
            "END:VCALENDAR\r\n",
        );
        assert!(forward_publish_payload(multiple).is_err());
        assert!(
            forward_publish_payload(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:event\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n"
            )
            .is_err()
        );

        let mut missing_uid = record("source-a", "uid", "Title", None);
        missing_uid.component = "BEGIN:VJOURNAL\r\nSUMMARY:Title\r\nEND:VJOURNAL\r\n".to_string();
        assert!(build_forward_publish_payload(&missing_uid).is_err());
        assert!(forward_publish_payload("BEGIN:VEVENT\r\nUID:event\r\nEND:VEVENT\r\n").is_err());

        let mut mismatch = original.clone();
        mismatch.component = component("other", "Title", None);
        assert!(build_forward_publish_payload(&mismatch).is_err());
    }

    #[test]
    fn component_parser_is_case_insensitive_but_rejects_trailing_or_nested_data() {
        let lower = "begin:vjournal\r\nuid:lower\r\nsummary:Title\r\nend:vjournal\r\n";
        assert_eq!(validate_memo_component(lower).unwrap().uid, "lower");
        assert!(
            validate_memo_component(
                "BEGIN:VJOURNAL\r\nUID:one\r\nEND:VJOURNAL\r\nX-TRAILING:value\r\n"
            )
            .is_err()
        );
        assert!(validate_memo_component(
            "BEGIN:VJOURNAL\r\nUID:one\r\nBEGIN:VJOURNAL\r\nUID:two\r\nEND:VJOURNAL\r\nEND:VJOURNAL\r\n"
        )
        .is_err());
    }

    #[test]
    fn print_and_preview_escape_values_and_keep_inline_attachments_inert() {
        let mut memo = record("source-a", "safe", "<script>", None);
        memo.component = component("safe", "<script>", None)
            .replace("DESCRIPTION:body", "DESCRIPTION:<script>\nline");
        let print = build_print_document(&memo).unwrap();
        assert!(print.html.contains("&lt;script&gt;"));
        assert!(!print.html.contains("<script>"));
        let mut inline = memo.clone();
        inline.component = inline.component.replace(
            "ATTACH;FMTTYPE=text/plain:URI:https://example.test/a",
            "ATTACH;VALUE=BINARY:PHNjcmlwdD4=",
        );
        let preview = preview_for_selection(&[inline], &[MemoIdentity::new("source-a", "safe")])
            .unwrap()
            .unwrap();
        assert!(
            preview
                .attachments
                .iter()
                .any(|attachment| attachment.inline && attachment.inert)
        );
        assert!(preview.markup.contains("inert"));
        assert!(preview.markup.contains("PHNjcmlwdD4="));
        assert!(preview_for_selection(&[], &[]).unwrap().is_none());
        assert!(
            preview_for_selection(&[memo.clone()], &[])
                .unwrap()
                .is_none()
        );
        assert!(
            preview_for_selection(
                &[memo.clone(), memo],
                &[
                    MemoIdentity::new("source-a", "safe"),
                    MemoIdentity::new("source-a", "safe")
                ]
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn multi_export_is_one_envelope_and_preserves_component_order() {
        let first = record("source-b", "b", "B", None);
        let second = record("source-a", "a", "A", None);
        let export = build_multi_export(
            &[first.clone(), second.clone()],
            &[MemoIdentity::from(&first), MemoIdentity::from(&second)],
        )
        .unwrap();
        assert_eq!(export.matches("BEGIN:VCALENDAR").count(), 1);
        assert_eq!(export.matches("END:VCALENDAR").count(), 1);
        assert!(export.contains("BEGIN:VALARM\r\n"));
        assert!(export.contains("X-ORDER:kept\r\n"));
        assert!(export.find("UID:a").unwrap() < export.find("UID:b").unwrap());
        assert!(first.component.contains("X-ORDER:kept"));
    }

    #[test]
    fn malformed_or_uid_mismatch_fails_before_batch_output() {
        let mut wrong = record("source-a", "row-uid", "Wrong", None);
        wrong.component = component("component-uid", "Wrong", None);
        assert!(build_multi_export(&[wrong.clone()], &[MemoIdentity::from(&wrong)]).is_err());
        assert!(plan_paste("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n", "source-a", true).is_err());
        assert!(
            plan_paste(
                &forward_publish_payload(&record("source-a", "one", "One", None).component)
                    .unwrap(),
                "source-a",
                false
            )
            .is_err()
        );
        let duplicate_payload = "BEGIN:VCALENDAR\r\nBEGIN:VJOURNAL\r\nUID:one\r\nSUMMARY:one\r\nEND:VJOURNAL\r\nBEGIN:VJOURNAL\r\nUID:one\r\nSUMMARY:again\r\nEND:VJOURNAL\r\nEND:VCALENDAR\r\n";
        assert!(plan_paste(duplicate_payload, "source-a", true).is_err());
        let existing = [record("source-a", "one", "Existing", None)];
        assert!(
            plan_paste_into(
                &build_clipboard_payload(
                    std::slice::from_ref(&existing[0]),
                    &[MemoIdentity::from(&existing[0])]
                )
                .unwrap(),
                "source-a",
                true,
                &existing,
            )
            .is_err()
        );
    }

    #[test]
    fn duplicate_same_source_identity_is_rejected_but_duplicate_uid_across_sources_is_not() {
        let first = record("source-a", "same", "A", None);
        let second = record("source-b", "same", "B", None);
        let export = build_multi_export(
            &[first.clone(), second.clone()],
            &[MemoIdentity::from(&first), MemoIdentity::from(&second)],
        )
        .expect("source is part of the stable identity");
        assert_eq!(export.matches("BEGIN:VJOURNAL").count(), 2);

        let duplicate = vec![first.clone(), first.clone()];
        assert!(build_multi_export(&duplicate, &[MemoIdentity::from(&first)]).is_err());
    }

    #[test]
    fn mixed_read_only_delete_is_atomic_preflight_and_paste_requires_vjournal_writable_source() {
        let writable = record("source-a", "w", "W", None);
        let read_only = record("", "r", "R", None);
        let result = plan_delete(
            &[writable.clone(), read_only.clone()],
            &[
                MemoIdentity::from(&writable),
                MemoIdentity::from(&read_only),
            ],
            |source| source == "source-a",
        );
        let errors = result.expect_err("mixed read-only delete must not start");
        assert!(errors.succeeded.is_empty());
        assert_eq!(errors.failed.len(), 1);
        assert!(
            plan_paste(
                "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:event\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
                "source-a",
                true,
            )
            .is_err()
        );
        assert!(
            plan_paste(
                &build_clipboard_payload(
                    std::slice::from_ref(&writable),
                    &[MemoIdentity::from(&writable)]
                )
                .unwrap(),
                "source-a",
                true,
            )
            .is_ok()
        );
    }

    #[test]
    fn preview_state_has_persistent_toggle_and_orientation_values() {
        let mut state = MemoPreviewState::default();
        assert!(state.visible);
        state.toggle();
        assert!(!state.visible);
        state.set_orientation(MemoPreviewOrientation::Vertical);
        assert_eq!(state.orientation.setting_value(), "vertical");
    }
}
