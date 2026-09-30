// SPDX-License-Identifier: GPL-3.0-or-later
//
// Calendar export service supporting iCalendar (.ics), CSV (.csv), and RDF (.rdf)
// with upstream Evolution `src/plugins/save-calendar/` parity.

use super::calendar_store_service::CalendarEventRecord;

/// Supported calendar export formats corresponding to upstream `save-calendar` plugin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalendarExportFormat {
    /// RFC 5545 iCalendar (`.ics`), upstream `ical-format.c`
    ICalendar,
    /// Delimited tabular text (`.csv`), upstream `csv-format.c`
    Csv,
    /// RSS 1.0 / RDF XML (`.rdf`), upstream `rdf-format.c`
    Rdf,
}

impl CalendarExportFormat {
    pub fn extension(&self) -> &'static str {
        match self {
            Self::ICalendar => "ics",
            Self::Csv => "csv",
            Self::Rdf => "rdf",
        }
    }

    pub fn mime_type(&self) -> &'static str {
        match self {
            Self::ICalendar => "text/calendar",
            Self::Csv => "text/csv",
            Self::Rdf => "application/rdf+xml",
        }
    }

    pub fn display_label(&self) -> &'static str {
        match self {
            Self::ICalendar => "iCalendar (.ics)",
            Self::Csv => "Kommagetrennte Werte (.csv)",
            Self::Rdf => "RDF / RSS 1.0 (.rdf)",
        }
    }
}

/// Formatting options for calendar CSV export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarCsvOptions {
    pub delimiter: String,
    pub quote: char,
    pub newline: String,
    pub include_header: bool,
}

impl Default for CalendarCsvOptions {
    fn default() -> Self {
        Self {
            delimiter: ",".to_string(),
            quote: '"',
            newline: "\r\n".to_string(),
            include_header: true,
        }
    }
}

/// Extracted properties from an event component or record.
#[derive(Debug, Clone, Default)]
struct ParsedEventFields {
    uid: String,
    summary: String,
    description: String,
    categories: Vec<String>,
    comments: Vec<String>,
    completed: String,
    created: String,
    contacts: Vec<String>,
    start: String,
    end: String,
    due: String,
    percent_done: String,
    priority: String,
    url: String,
    attendees: Vec<String>,
    location: String,
    modified: String,
}

/// Unfold raw iCalendar lines and extract properties.
fn parse_event_fields(record: &CalendarEventRecord) -> ParsedEventFields {
    let mut fields = ParsedEventFields {
        uid: record.uid.clone(),
        summary: record.summary.clone(),
        description: record.description.clone(),
        location: record.location.clone(),
        start: record.start.clone(),
        end: record.end.clone(),
        ..ParsedEventFields::default()
    };

    if record.component.trim().is_empty() {
        return fields;
    }

    // Unfold lines
    let mut unfolded_lines: Vec<String> = Vec::new();
    for line in record.component.lines() {
        let trimmed_end = line.trim_end_matches(['\r', '\n']);
        if (trimmed_end.starts_with(' ') || trimmed_end.starts_with('\t'))
            && !unfolded_lines.is_empty()
        {
            if let Some(last) = unfolded_lines.last_mut() {
                last.push_str(&trimmed_end[1..]);
            }
        } else if !trimmed_end.is_empty() {
            unfolded_lines.push(trimmed_end.to_string());
        }
    }

    for line in unfolded_lines {
        let Some((prop_with_params, val)) = line.split_once(':') else {
            continue;
        };
        let prop_name = prop_with_params
            .split([';', ':'])
            .next()
            .unwrap_or("")
            .trim()
            .to_ascii_uppercase();
        let value = val.trim();

        match prop_name.as_str() {
            "UID" if fields.uid.is_empty() => fields.uid = unescape_ical_field(value),
            "SUMMARY" if fields.summary.is_empty() => fields.summary = unescape_ical_field(value),
            "DESCRIPTION" if fields.description.is_empty() => {
                fields.description = unescape_ical_field(value)
            }
            "LOCATION" if fields.location.is_empty() => {
                fields.location = unescape_ical_field(value)
            }
            "CATEGORIES" => {
                for cat in value.split(',') {
                    let cat = unescape_ical_field(cat.trim());
                    if !cat.is_empty() {
                        fields.categories.push(cat);
                    }
                }
            }
            "COMMENT" => fields.comments.push(unescape_ical_field(value)),
            "COMPLETED" => fields.completed = value.to_string(),
            "CREATED" => fields.created = value.to_string(),
            "LAST-MODIFIED" => fields.modified = value.to_string(),
            "CONTACT" => fields.contacts.push(unescape_ical_field(value)),
            "DTSTART" if fields.start.is_empty() => fields.start = value.to_string(),
            "DTEND" if fields.end.is_empty() => fields.end = value.to_string(),
            "DUE" => fields.due = value.to_string(),
            "PERCENT-COMPLETE" => fields.percent_done = value.to_string(),
            "PRIORITY" => fields.priority = value.to_string(),
            "URL" => fields.url = value.to_string(),
            "ATTENDEE" => {
                let mail = if let Some((_, mail_addr)) = value.split_once("mailto:") {
                    mail_addr.trim().to_string()
                } else {
                    value.to_string()
                };
                fields.attendees.push(mail);
            }
            _ => {}
        }
    }

    fields
}

fn unescape_ical_field(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            match chars.peek() {
                Some('n' | 'N') => {
                    chars.next();
                    out.push('\n');
                }
                Some('\\') => {
                    chars.next();
                    out.push('\\');
                }
                Some(';') => {
                    chars.next();
                    out.push(';');
                }
                Some(',') => {
                    chars.next();
                    out.push(',');
                }
                _ => out.push('\\'),
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Format CSV cell with proper quoting and escaping according to RFC 4180 / Evolution csv-format.c
fn escape_csv_cell(cell: &str, delimiter: &str, quote: char) -> String {
    let quote_str = quote.to_string();
    let needs_quote = cell.contains(delimiter)
        || cell.contains(quote)
        || cell.contains(';')
        || cell.contains('\n')
        || cell.contains('\r')
        || cell.starts_with(' ')
        || cell.ends_with(' ');

    if needs_quote {
        let escaped = cell.replace(&quote_str, &format!("{quote}{quote}"));
        format!("{quote}{escaped}{quote}")
    } else {
        cell.to_string()
    }
}

/// Escape text for XML output (RDF format).
fn escape_xml(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

/// Export a collection of calendar events into the chosen format.
pub fn export_calendar_events(
    events: &[CalendarEventRecord],
    format: CalendarExportFormat,
    options: Option<CalendarCsvOptions>,
) -> Result<String, String> {
    if events.is_empty() {
        return Err("Keine Kalendereinträge zum Exportieren vorhanden".to_string());
    }

    match format {
        CalendarExportFormat::ICalendar => export_to_icalendar(events),
        CalendarExportFormat::Csv => {
            let opts = options.unwrap_or_default();
            export_to_csv(events, &opts)
        }
        CalendarExportFormat::Rdf => export_to_rdf(events),
    }
}

/// Export to multi-component VCALENDAR.
fn export_to_icalendar(events: &[CalendarEventRecord]) -> Result<String, String> {
    let mut components = Vec::new();

    for event in events {
        let trimmed = event.component.trim();
        if trimmed.is_empty() {
            // Synthesize minimal VEVENT if component text is missing
            let uid = if event.uid.trim().is_empty() {
                uuid::Uuid::new_v4().to_string()
            } else {
                event.uid.trim().to_string()
            };
            let mut synth = format!(
                "BEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{}\r\n",
                event.summary.trim()
            );
            if !event.start.trim().is_empty() {
                synth.push_str(&format!("DTSTART:{}\r\n", event.start.trim()));
            }
            if !event.end.trim().is_empty() {
                synth.push_str(&format!("DTEND:{}\r\n", event.end.trim()));
            }
            if !event.location.trim().is_empty() {
                synth.push_str(&format!("LOCATION:{}\r\n", event.location.trim()));
            }
            if !event.description.trim().is_empty() {
                synth.push_str(&format!("DESCRIPTION:{}\r\n", event.description.trim()));
            }
            synth.push_str("END:VEVENT");
            components.push(synth);
        } else if trimmed.starts_with("BEGIN:VCALENDAR") {
            // Extract inner components
            let inner = extract_inner_components(trimmed);
            if inner.is_empty() {
                components.push(trimmed.to_string());
            } else {
                components.extend(inner);
            }
        } else {
            components.push(trimmed.to_string());
        }
    }

    let mut output =
        String::from("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//Meridian//EN\r\n");
    for comp in components {
        output.push_str(&comp);
        if !comp.ends_with("\r\n") {
            output.push_str("\r\n");
        }
    }
    output.push_str("END:VCALENDAR\r\n");
    Ok(output)
}

fn extract_inner_components(vcalendar: &str) -> Vec<String> {
    let mut comps = Vec::new();
    let mut current_comp = Vec::new();
    let mut inside = false;

    for line in vcalendar.lines() {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let upper = trimmed.to_ascii_uppercase();
        if upper == "BEGIN:VEVENT"
            || upper == "BEGIN:VTODO"
            || upper == "BEGIN:VJOURNAL"
            || upper == "BEGIN:VTIMEZONE"
        {
            inside = true;
            current_comp.push(trimmed);
        } else if inside {
            current_comp.push(trimmed);
            if upper == "END:VEVENT"
                || upper == "END:VTODO"
                || upper == "END:VJOURNAL"
                || upper == "END:VTIMEZONE"
            {
                comps.push(current_comp.join("\r\n"));
                current_comp.clear();
                inside = false;
            }
        }
    }
    comps
}

/// Export to CSV matching upstream Evolution `csv-format.c`.
/// Columns: UID, Summary, Description List, Categories List, Comment List,
/// Completed, Created, Contact List, Start, End, Due, percent Done, Priority,
/// URL, Attendees List, Location, Modified
fn export_to_csv(
    events: &[CalendarEventRecord],
    options: &CalendarCsvOptions,
) -> Result<String, String> {
    let mut output = String::new();

    if options.include_header {
        let headers = [
            "UID",
            "Summary",
            "Description List",
            "Categories List",
            "Comment List",
            "Completed",
            "Created",
            "Contact List",
            "Start",
            "End",
            "Due",
            "percent Done",
            "Priority",
            "URL",
            "Attendees List",
            "Location",
            "Modified",
        ];
        let header_row = headers
            .iter()
            .map(|h| escape_csv_cell(h, &options.delimiter, options.quote))
            .collect::<Vec<_>>()
            .join(&options.delimiter);
        output.push_str(&header_row);
        output.push_str(&options.newline);
    }

    for event in events {
        let f = parse_event_fields(event);
        let cells = [
            escape_csv_cell(&f.uid, &options.delimiter, options.quote),
            escape_csv_cell(&f.summary, &options.delimiter, options.quote),
            escape_csv_cell(&f.description, &options.delimiter, options.quote),
            escape_csv_cell(&f.categories.join("; "), &options.delimiter, options.quote),
            escape_csv_cell(&f.comments.join("; "), &options.delimiter, options.quote),
            escape_csv_cell(&f.completed, &options.delimiter, options.quote),
            escape_csv_cell(&f.created, &options.delimiter, options.quote),
            escape_csv_cell(&f.contacts.join("; "), &options.delimiter, options.quote),
            escape_csv_cell(&f.start, &options.delimiter, options.quote),
            escape_csv_cell(&f.end, &options.delimiter, options.quote),
            escape_csv_cell(&f.due, &options.delimiter, options.quote),
            escape_csv_cell(&f.percent_done, &options.delimiter, options.quote),
            escape_csv_cell(&f.priority, &options.delimiter, options.quote),
            escape_csv_cell(&f.url, &options.delimiter, options.quote),
            escape_csv_cell(&f.attendees.join("; "), &options.delimiter, options.quote),
            escape_csv_cell(&f.location, &options.delimiter, options.quote),
            escape_csv_cell(&f.modified, &options.delimiter, options.quote),
        ];
        output.push_str(&cells.join(&options.delimiter));
        output.push_str(&options.newline);
    }

    Ok(output)
}

/// Export to RDF / RSS 1.0 XML matching upstream Evolution `rdf-format.c`.
fn export_to_rdf(events: &[CalendarEventRecord]) -> Result<String, String> {
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    out.push_str("<rdf:RDF\n");
    out.push_str("  xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"\n");
    out.push_str("  xmlns=\"http://purl.org/rss/1.0/\"\n");
    out.push_str("  xmlns:ev=\"http://purl.org/rss/1.0/modules/event/\"\n");
    out.push_str("  xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n");

    // Channel metadata
    out.push_str("  <channel rdf:about=\"https://www.gnome.org/projects/evolution/\">\n");
    out.push_str("    <title>Kalendertermine</title>\n");
    out.push_str("    <link>https://www.gnome.org/projects/evolution/</link>\n");
    out.push_str("    <description>Meridian Kalender-Export</description>\n");
    out.push_str("    <items>\n");
    out.push_str("      <rdf:Seq>\n");
    for event in events {
        let f = parse_event_fields(event);
        let resource = if !f.url.is_empty() {
            escape_xml(&f.url)
        } else if !f.uid.is_empty() {
            format!("urn:uuid:{}", escape_xml(&f.uid))
        } else {
            format!("urn:uuid:{}", uuid::Uuid::new_v4())
        };
        out.push_str(&format!("        <rdf:li rdf:resource=\"{resource}\"/>\n"));
    }
    out.push_str("      </rdf:Seq>\n");
    out.push_str("    </items>\n");
    out.push_str("  </channel>\n");

    // Item entries
    for event in events {
        let f = parse_event_fields(event);
        let resource = if !f.url.is_empty() {
            escape_xml(&f.url)
        } else if !f.uid.is_empty() {
            format!("urn:uuid:{}", escape_xml(&f.uid))
        } else {
            format!("urn:uuid:{}", uuid::Uuid::new_v4())
        };

        out.push_str(&format!("  <item rdf:about=\"{resource}\">\n"));
        out.push_str(&format!("    <title>{}</title>\n", escape_xml(&f.summary)));
        if !f.description.is_empty() {
            out.push_str(&format!(
                "    <description>{}</description>\n",
                escape_xml(&f.description)
            ));
        }
        if !f.start.is_empty() {
            out.push_str(&format!(
                "    <ev:startdate>{}</ev:startdate>\n",
                escape_xml(&f.start)
            ));
        }
        if !f.end.is_empty() {
            out.push_str(&format!(
                "    <ev:enddate>{}</ev:enddate>\n",
                escape_xml(&f.end)
            ));
        }
        if !f.location.is_empty() {
            out.push_str(&format!(
                "    <ev:location>{}</ev:location>\n",
                escape_xml(&f.location)
            ));
        }
        out.push_str("  </item>\n");
    }

    out.push_str("</rdf:RDF>\n");
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_events() -> Vec<CalendarEventRecord> {
        vec![
            CalendarEventRecord {
                uid: "event-1@evolution.example".to_string(),
                source_uid: "local-calendar".to_string(),
                source_display_name: "Persönlich".to_string(),
                component: "BEGIN:VEVENT\r\nUID:event-1@evolution.example\r\nSUMMARY:Team Meeting\r\nLOCATION:Conference Room\r\nDESCRIPTION:Weekly sync, discuss roadmap\r\nCATEGORIES:Work,Planning\r\nDTSTART:20260910T090000Z\r\nDTEND:20260910T100000Z\r\nPRIORITY:1\r\nURL:https://meet.example.com/sync\r\nATTENDEE;CN=Ada:mailto:ada@example.com\r\nEND:VEVENT".to_string(),
                summary: "Team Meeting".to_string(),
                start: "20260910T090000Z".to_string(),
                end: "20260910T100000Z".to_string(),
                location: "Conference Room".to_string(),
                description: "Weekly sync, discuss roadmap".to_string(),
                all_day: false,
            },
            CalendarEventRecord {
                uid: "event-2@evolution.example".to_string(),
                source_uid: "local-calendar".to_string(),
                source_display_name: "Persönlich".to_string(),
                component: "BEGIN:VEVENT\r\nUID:event-2@evolution.example\r\nSUMMARY:Doctor Appointment\r\nLOCATION:Downtown Clinic\r\nDESCRIPTION:Annual checkup\r\nCATEGORIES:Personal\r\nDTSTART:20260912T140000Z\r\nDTEND:20260912T150000Z\r\nEND:VEVENT".to_string(),
                summary: "Doctor Appointment".to_string(),
                start: "20260912T140000Z".to_string(),
                end: "20260912T150000Z".to_string(),
                location: "Downtown Clinic".to_string(),
                description: "Annual checkup".to_string(),
                all_day: false,
            },
        ]
    }

    #[test]
    fn export_calendar_events_ics_creates_valid_vcalendar_envelope() {
        let events = sample_events();
        let ics = export_calendar_events(&events, CalendarExportFormat::ICalendar, None).unwrap();

        assert!(ics.starts_with("BEGIN:VCALENDAR\r\n"));
        assert!(ics.ends_with("END:VCALENDAR\r\n"));
        assert!(ics.contains("VERSION:2.0"));
        assert!(ics.contains("UID:event-1@evolution.example"));
        assert!(ics.contains("SUMMARY:Team Meeting"));
        assert!(ics.contains("UID:event-2@evolution.example"));
        assert!(ics.contains("SUMMARY:Doctor Appointment"));
    }

    #[test]
    fn export_calendar_events_csv_matches_upstream_csv_format_columns_and_escaping() {
        let events = sample_events();
        let csv = export_calendar_events(&events, CalendarExportFormat::Csv, None).unwrap();

        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines.len() >= 3);
        assert_eq!(
            lines[0],
            "UID,Summary,Description List,Categories List,Comment List,Completed,Created,Contact List,Start,End,Due,percent Done,Priority,URL,Attendees List,Location,Modified"
        );

        // First row contains team meeting with quotes around description due to comma
        assert!(lines[1].contains("event-1@evolution.example"));
        assert!(lines[1].contains("Team Meeting"));
        assert!(lines[1].contains("\"Weekly sync, discuss roadmap\""));
        assert!(lines[1].contains("\"Work; Planning\""));
        assert!(lines[1].contains("ada@example.com"));

        // Second row
        assert!(lines[2].contains("event-2@evolution.example"));
        assert!(lines[2].contains("Doctor Appointment"));
    }

    #[test]
    fn export_calendar_events_rdf_generates_valid_rss_rdf_xml() {
        let events = sample_events();
        let rdf = export_calendar_events(&events, CalendarExportFormat::Rdf, None).unwrap();

        assert!(rdf.contains("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(rdf.contains("<rdf:RDF"));
        assert!(rdf.contains("<channel rdf:about=\"https://www.gnome.org/projects/evolution/\">"));
        assert!(rdf.contains("<rdf:li rdf:resource=\"https://meet.example.com/sync\"/>"));
        assert!(rdf.contains("<rdf:li rdf:resource=\"urn:uuid:event-2@evolution.example\"/>"));
        assert!(rdf.contains("<item rdf:about=\"https://meet.example.com/sync\">"));
        assert!(rdf.contains("<title>Team Meeting</title>"));
        assert!(rdf.contains("<ev:location>Conference Room</ev:location>"));
        assert!(rdf.contains("</rdf:RDF>"));
    }

    #[test]
    fn export_calendar_events_handles_empty_selection_fail_closed() {
        let empty: Vec<CalendarEventRecord> = Vec::new();
        assert!(export_calendar_events(&empty, CalendarExportFormat::ICalendar, None).is_err());
        assert!(export_calendar_events(&empty, CalendarExportFormat::Csv, None).is_err());
        assert!(export_calendar_events(&empty, CalendarExportFormat::Rdf, None).is_err());
    }

    #[test]
    fn export_calendar_csv_custom_delimiter_and_header_toggle() {
        let events = sample_events();
        let options = CalendarCsvOptions {
            delimiter: ";".to_string(),
            quote: '"',
            newline: "\n".to_string(),
            include_header: false,
        };
        let csv =
            export_calendar_events(&events, CalendarExportFormat::Csv, Some(options)).unwrap();

        let lines: Vec<&str> = csv.lines().collect();
        // No header line, so only 2 data lines
        assert_eq!(lines.len(), 2);
        assert!(lines[0].starts_with("event-1@evolution.example;Team Meeting"));
    }
}
