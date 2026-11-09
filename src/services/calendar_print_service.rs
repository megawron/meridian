// SPDX-License-Identifier: GPL-3.0-or-later
//
// Calendar event and schedule print service generating deterministic HTML and plain text
// print documents with CSS styling for single events and schedule overviews.

use std::collections::BTreeMap;

use super::calendar_store_service::CalendarEventRecord;

/// Deterministic, self-contained calendar print document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalendarPrintDocument {
    pub html: String,
    pub plain_text: String,
    pub event_count: usize,
}

fn escape_html(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            other => out.push(other),
        }
    }
    out
}

fn format_date_readable(raw: &str) -> String {
    let raw = raw.trim().trim_end_matches('Z');
    if raw.len() == 8 && raw.chars().all(|c| c.is_ascii_digit()) {
        return format!("{}.{}.{}", &raw[6..8], &raw[4..6], &raw[..4]);
    }
    if raw.len() >= 15 && raw.as_bytes().get(8) == Some(&b'T') {
        return format!(
            "{}.{}.{} {:02}:{:02}",
            &raw[6..8],
            &raw[4..6],
            &raw[..4],
            &raw[9..11],
            &raw[11..13]
        );
    }
    raw.to_string()
}

fn extract_date_key(raw: &str) -> String {
    let raw = raw.trim().trim_end_matches('Z');
    if raw.len() >= 8 && raw[..8].chars().all(|c| c.is_ascii_digit()) {
        format!("{}-{}-{}", &raw[..4], &raw[4..6], &raw[6..8])
    } else {
        "Undated".to_string()
    }
}

/// Build a print document for a single calendar event.
pub fn build_event_print_document(
    event: &CalendarEventRecord,
) -> Result<CalendarPrintDocument, String> {
    if event.summary.trim().is_empty() && event.component.trim().is_empty() {
        return Err("Cannot print an empty event".to_string());
    }

    let summary = if !event.summary.trim().is_empty() {
        event.summary.trim()
    } else {
        "Untitled Event"
    };

    let start_fmt = format_date_readable(&event.start);
    let end_fmt = format_date_readable(&event.end);
    let location = event.location.trim();
    let description = event.description.trim();
    let source_name = if !event.source_display_name.trim().is_empty() {
        event.source_display_name.trim()
    } else {
        "Local Calendar"
    };

    // Build HTML
    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str(&format!("<title>{}</title>\n", escape_html(summary)));
    html.push_str("<style>\n");
    html.push_str("body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif; margin: 24px; color: #1e293b; line-height: 1.5; }\n");
    html.push_str(".card { border: 1px solid #cbd5e1; border-radius: 8px; padding: 24px; max-width: 680px; margin: 0 auto; box-shadow: 0 1px 3px rgba(0,0,0,0.1); }\n");
    html.push_str(".header { border-bottom: 2px solid #3b82f6; padding-bottom: 12px; margin-bottom: 20px; }\n");
    html.push_str(".title { font-size: 24px; font-weight: bold; margin: 0; color: #0f172a; }\n");
    html.push_str(".badge { display: inline-block; background: #e0f2fe; color: #0369a1; font-size: 12px; font-weight: 600; padding: 2px 8px; border-radius: 4px; margin-top: 6px; }\n");
    html.push_str(".grid { display: grid; grid-template-columns: 140px 1fr; gap: 8px 16px; margin-bottom: 20px; }\n");
    html.push_str(".label { font-weight: 600; color: #64748b; }\n");
    html.push_str(".value { color: #1e293b; }\n");
    html.push_str(".description-box { background: #f8fafc; border-left: 4px solid #94a3b8; padding: 12px 16px; margin-top: 16px; border-radius: 0 4px 4px 0; white-space: pre-wrap; }\n");
    html.push_str("@media print { body { margin: 0; } .card { box-shadow: none; border: 1px solid #94a3b8; } }\n");
    html.push_str("</style>\n</head>\n<body>\n");

    html.push_str("<div class=\"card\">\n");
    html.push_str("  <div class=\"header\">\n");
    html.push_str(&format!(
        "    <h1 class=\"title\">{}</h1>\n",
        escape_html(summary)
    ));
    html.push_str(&format!(
        "    <div class=\"badge\">{}</div>\n",
        escape_html(source_name)
    ));
    if event.all_day {
        html.push_str("    <div class=\"badge\" style=\"background:#fef3c7;color:#b45309;\">All-day</div>\n");
    }
    html.push_str("  </div>\n");

    html.push_str("  <div class=\"grid\">\n");
    if !start_fmt.is_empty() {
        html.push_str("    <div class=\"label\">Start:</div>\n");
        html.push_str(&format!(
            "    <div class=\"value\">{}</div>\n",
            escape_html(&start_fmt)
        ));
    }
    if !end_fmt.is_empty() {
        html.push_str("    <div class=\"label\">End:</div>\n");
        html.push_str(&format!(
            "    <div class=\"value\">{}</div>\n",
            escape_html(&end_fmt)
        ));
    }
    if !location.is_empty() {
        html.push_str("    <div class=\"label\">Location:</div>\n");
        html.push_str(&format!(
            "    <div class=\"value\">{}</div>\n",
            escape_html(location)
        ));
    }
    html.push_str("  </div>\n");

    if !description.is_empty() {
        html.push_str("  <div class=\"label\">Description:</div>\n");
        html.push_str(&format!(
            "  <div class=\"description-box\">{}</div>\n",
            escape_html(description)
        ));
    }

    html.push_str("</div>\n</body>\n</html>\n");

    // Build plain text
    let mut plain = String::new();
    plain.push_str(&format!("=== {} ===\n", summary));
    plain.push_str(&format!("Calendar: {}\n", source_name));
    if event.all_day {
        plain.push_str("Type: All-day Event\n");
    }
    if !start_fmt.is_empty() {
        plain.push_str(&format!("Start:    {}\n", start_fmt));
    }
    if !end_fmt.is_empty() {
        plain.push_str(&format!("End:      {}\n", end_fmt));
    }
    if !location.is_empty() {
        plain.push_str(&format!("Location: {}\n", location));
    }
    if !description.is_empty() {
        plain.push_str("\nDescription:\n");
        plain.push_str(description);
        plain.push('\n');
    }

    Ok(CalendarPrintDocument {
        html,
        plain_text: plain,
        event_count: 1,
    })
}

/// Build a print document for a list of calendar events (agenda / schedule).
pub fn build_calendar_list_print_document(
    events: &[CalendarEventRecord],
    title: Option<&str>,
) -> Result<CalendarPrintDocument, String> {
    if events.is_empty() {
        return Err("No events available to print".to_string());
    }

    let page_title = title.unwrap_or("Schedule Overview");

    // Sort events chronologically by start, then summary
    let mut sorted = events.to_vec();
    sorted.sort_by(|a, b| match a.start.cmp(&b.start) {
        std::cmp::Ordering::Equal => a.summary.cmp(&b.summary),
        other => other,
    });

    // Group by date
    let mut grouped: BTreeMap<String, Vec<&CalendarEventRecord>> = BTreeMap::new();
    for event in &sorted {
        let key = extract_date_key(&event.start);
        grouped.entry(key).or_default().push(event);
    }

    // Build HTML
    let mut html = String::new();
    html.push_str("<!DOCTYPE html>\n<html>\n<head>\n<meta charset=\"utf-8\">\n");
    html.push_str(&format!("<title>{}</title>\n", escape_html(page_title)));
    html.push_str("<style>\n");
    html.push_str("body { font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Helvetica, Arial, sans-serif; margin: 24px; color: #1e293b; line-height: 1.4; }\n");
    html.push_str(".agenda-header { border-bottom: 2px solid #2563eb; padding-bottom: 8px; margin-bottom: 24px; display: flex; justify-content: space-between; align-items: baseline; }\n");
    html.push_str(
        ".agenda-title { font-size: 22px; font-weight: bold; margin: 0; color: #0f172a; }\n",
    );
    html.push_str(".agenda-count { font-size: 13px; color: #64748b; }\n");
    html.push_str(".day-group { margin-bottom: 24px; break-inside: avoid; }\n");
    html.push_str(".day-header { font-size: 15px; font-weight: 700; color: #1e40af; background: #eff6ff; padding: 6px 12px; border-radius: 4px; margin-bottom: 8px; }\n");
    html.push_str("table { width: 100%; border-collapse: collapse; margin-bottom: 8px; }\n");
    html.push_str("th, td { text-align: left; padding: 8px 12px; border-bottom: 1px solid #e2e8f0; vertical-align: top; }\n");
    html.push_str(
        "th { color: #64748b; font-size: 12px; text-transform: uppercase; font-weight: 600; }\n",
    );
    html.push_str(
        ".time-col { width: 150px; font-weight: 600; color: #334155; font-size: 13px; }\n",
    );
    html.push_str(".summary-col { font-weight: 600; color: #0f172a; }\n");
    html.push_str(".desc { font-size: 12px; color: #64748b; margin-top: 2px; }\n");
    html.push_str(".loc { font-size: 12px; color: #0284c7; margin-top: 2px; }\n");
    html.push_str(
        "@media print { body { margin: 0; } .day-group { page-break-inside: avoid; } }\n",
    );
    html.push_str("</style>\n</head>\n<body>\n");

    html.push_str("<div class=\"agenda-header\">\n");
    html.push_str(&format!(
        "  <h1 class=\"agenda-title\">{}</h1>\n",
        escape_html(page_title)
    ));
    html.push_str(&format!(
        "  <div class=\"agenda-count\">{} event(s)</div>\n",
        sorted.len()
    ));
    html.push_str("</div>\n");

    // Build plain text
    let mut plain = String::new();
    plain.push_str(&format!("=== {} ===\n", page_title));
    plain.push_str(&format!("Total: {} event(s)\n\n", sorted.len()));

    for (day_key, day_events) in grouped {
        html.push_str("<div class=\"day-group\">\n");
        html.push_str(&format!(
            "  <div class=\"day-header\">{}</div>\n",
            escape_html(&day_key)
        ));
        html.push_str("  <table>\n");
        html.push_str("    <thead><tr><th class=\"time-col\">Time</th><th>Event</th><th>Location</th></tr></thead>\n");
        html.push_str("    <tbody>\n");

        plain.push_str(&format!("--- {} ---\n", day_key));

        for event in day_events {
            let start = format_date_readable(&event.start);
            let end = format_date_readable(&event.end);
            let time_str = if event.all_day {
                "All-day".to_string()
            } else if !start.is_empty() && !end.is_empty() {
                format!("{start} – {end}")
            } else if !start.is_empty() {
                start.clone()
            } else {
                "No time".to_string()
            };

            let loc = event.location.trim();
            let desc = event.description.trim();

            html.push_str("    <tr>\n");
            html.push_str(&format!(
                "      <td class=\"time-col\">{}</td>\n",
                escape_html(&time_str)
            ));
            html.push_str("      <td>\n");
            html.push_str(&format!(
                "        <div class=\"summary-col\">{}</div>\n",
                escape_html(&event.summary)
            ));
            if !desc.is_empty() {
                html.push_str(&format!(
                    "        <div class=\"desc\">{}</div>\n",
                    escape_html(desc)
                ));
            }
            html.push_str("      </td>\n");
            html.push_str(&format!(
                "      <td class=\"loc\">{}</td>\n",
                escape_html(loc)
            ));
            html.push_str("    </tr>\n");

            plain.push_str(&format!("  [{}] {}\n", time_str, event.summary.trim()));
            if !loc.is_empty() {
                plain.push_str(&format!("      Location: {}\n", loc));
            }
            if !desc.is_empty() {
                plain.push_str(&format!("      Note: {}\n", desc));
            }
        }

        html.push_str("    </tbody>\n");
        html.push_str("  </table>\n");
        html.push_str("</div>\n");
        plain.push('\n');
    }

    html.push_str("</body>\n</html>\n");

    Ok(CalendarPrintDocument {
        html,
        plain_text: plain,
        event_count: sorted.len(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_records() -> Vec<CalendarEventRecord> {
        vec![
            CalendarEventRecord {
                uid: "event-1".to_string(),
                source_uid: "local".to_string(),
                source_display_name: "Personal".to_string(),
                component: String::new(),
                summary: "Dentist".to_string(),
                start: "20260915T093000Z".to_string(),
                end: "20260915T103000Z".to_string(),
                location: "Praxis Dr. Müller".to_string(),
                description: "Checkup & Prophylaxis".to_string(),
                all_day: false,
            },
            CalendarEventRecord {
                uid: "event-2".to_string(),
                source_uid: "local".to_string(),
                source_display_name: "Work".to_string(),
                component: String::new(),
                summary: "Project Review <Sprint 42> & Roadmap".to_string(),
                start: "20260915T140000Z".to_string(),
                end: "20260915T150000Z".to_string(),
                location: "Room Newton".to_string(),
                description: "Present results".to_string(),
                all_day: false,
            },
        ]
    }

    #[test]
    fn build_event_print_document_escapes_html_and_formats_times() {
        let records = sample_records();
        let doc = build_event_print_document(&records[1]).unwrap();

        assert_eq!(doc.event_count, 1);
        assert!(
            doc.html
                .contains("Project Review &lt;Sprint 42&gt; &amp; Roadmap")
        );
        assert!(doc.html.contains("Room Newton"));
        assert!(doc.html.contains("Present results"));
        assert!(
            doc.plain_text
                .contains("Project Review <Sprint 42> & Roadmap")
        );
        assert!(doc.plain_text.contains("Room Newton"));
    }

    #[test]
    fn build_calendar_list_print_document_sorts_and_groups_by_date() {
        let records = sample_records();
        let doc = build_calendar_list_print_document(&records, Some("Weekly Schedule")).unwrap();

        assert_eq!(doc.event_count, 2);
        assert!(doc.html.contains("Weekly Schedule"));
        assert!(doc.html.contains("2 event(s)"));
        assert!(doc.html.contains("2026-09-15"));
        assert!(doc.html.contains("Dentist"));
        assert!(doc.html.contains("Praxis Dr. Müller"));
        assert!(
            doc.html
                .contains("Project Review &lt;Sprint 42&gt; &amp; Roadmap")
        );

        assert!(doc.plain_text.contains("=== Weekly Schedule ==="));
        assert!(doc.plain_text.contains("--- 2026-09-15 ---"));
        assert!(doc.plain_text.contains("Dentist"));
    }

    #[test]
    fn build_calendar_list_print_document_empty_returns_err() {
        assert!(build_calendar_list_print_document(&[], None).is_err());
    }
}
