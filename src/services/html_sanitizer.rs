// SPDX-License-Identifier: GPL-3.0-or-later
//
// Small, dependency-free sanitizer for HTML mail before it reaches WebKit.
//
// WebKit remains sandboxed and JavaScript-disabled, but untrusted mail is
// still reduced here so that event handlers, active embeds, CSS resource
// loads, and dangerous URI schemes do not enter the document at all.

const ALLOWED_TAGS: &[&str] = &[
    "a",
    "article",
    "b",
    "blockquote",
    "br",
    "caption",
    "center",
    "code",
    "col",
    "colgroup",
    "dd",
    "del",
    "details",
    "div",
    "dl",
    "dt",
    "em",
    "figcaption",
    "figure",
    "font",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "head",
    "hr",
    "html",
    "i",
    "img",
    "ins",
    "li",
    "main",
    "mark",
    "ol",
    "p",
    "pre",
    "section",
    "small",
    "span",
    "strong",
    "sub",
    "summary",
    "sup",
    "table",
    "tbody",
    "td",
    "tfoot",
    "th",
    "thead",
    "time",
    "tr",
    "u",
    "ul",
    "body",
];

const BLOCKED_TAGS: &[&str] = &[
    "applet", "base", "embed", "form", "iframe", "link", "meta", "object", "script", "style", "svg",
];

/// Sanitize untrusted mail HTML while preserving common formatting tags.
///
/// Remote image URLs are retained so the WebKit setting can block them by
/// default and the viewer's explicit “Bilder laden” action can enable them.
/// Dangerous URI schemes are removed regardless of the image policy.
pub fn sanitize_html(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;

    while cursor < input.len() {
        let Some(relative_start) = input[cursor..].find('<') else {
            output.push_str(&input[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        output.push_str(&input[cursor..start]);

        if input[start..].starts_with("<!--") {
            if let Some(end) = input[start + 4..].find("-->") {
                cursor = start + 4 + end + 3;
            } else {
                break;
            }
            continue;
        }

        let Some(end) = find_tag_end(input, start) else {
            output.push_str(&input[start..]);
            break;
        };
        let raw_tag = &input[start..=end];
        let Some((closing, tag_name)) = tag_name(raw_tag) else {
            cursor = end + 1;
            continue;
        };

        // Mail clients commonly keep layout rules in a <style> block. Keep
        // only a dependency-free, non-resource-loading subset so tables,
        // spacing and typography remain readable without allowing CSS to
        // fetch resources or execute browser-specific behavior.
        if tag_name == "style" {
            if !closing {
                if let Some((css, after_block)) = find_style_block_end(input, end + 1) {
                    if let Some(css) = sanitize_css(css) {
                        output.push_str("<style>");
                        output.push_str(&css);
                        output.push_str("</style>");
                    }
                    cursor = after_block;
                } else {
                    cursor = input.len();
                }
            } else {
                cursor = end + 1;
            }
            continue;
        }

        if BLOCKED_TAGS.contains(&tag_name.as_str()) {
            // Void HTML tags such as <meta>, <link>, <base> and <embed> do
            // not have a closing tag. Treating them like container tags
            // would skip the rest of an otherwise valid mail document when
            // find_block_end cannot find </tag>.
            let is_container = matches!(
                tag_name.as_str(),
                "applet" | "form" | "iframe" | "object" | "script" | "style" | "svg"
            );
            if !closing && is_container {
                cursor = find_block_end(input, end + 1, &tag_name).unwrap_or(input.len());
            } else {
                cursor = end + 1;
            }
            continue;
        }

        if let Some(safe_tag) = sanitize_tag(raw_tag, closing, &tag_name) {
            output.push_str(&safe_tag);
        }
        cursor = end + 1;
    }

    output
}

/// Remove externally hosted image elements from already sanitized mail HTML.
///
/// WebKit's image preference and the document CSP remain active in the
/// viewer, but removing remote elements here makes the privacy decision
/// deterministic across WebKit versions and avoids cached images appearing
/// before the user explicitly allows remote content. Inline `cid:` and
/// `data:` images are intentionally retained.
pub fn block_remote_images(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;

    while cursor < input.len() {
        let Some(relative_start) = input[cursor..].find('<') else {
            output.push_str(&input[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        output.push_str(&input[cursor..start]);

        let Some(end) = find_tag_end(input, start) else {
            output.push_str(&input[start..]);
            break;
        };
        let raw_tag = &input[start..=end];
        let is_remote_image = tag_name(raw_tag)
            .is_some_and(|(closing, tag)| !closing && tag == "img")
            && image_source(raw_tag).is_some_and(|source| is_remote_image_uri(&source));
        if !is_remote_image {
            output.push_str(raw_tag);
        }
        cursor = end + 1;
    }

    output
}

/// Return the readable text represented by an HTML mail body.
///
/// Sanitizing first removes active content and blocked container bodies, then
/// the remaining markup is converted to text with line breaks at common block
/// boundaries.  This is shared by the viewer and search/filter paths so a
/// search uses the same content the user can actually read.
pub fn html_to_plain_text(value: &str) -> String {
    let sanitized = strip_style_blocks(&sanitize_html(value));
    let mut output = String::with_capacity(sanitized.len());
    let mut in_tag = false;
    let mut tag = String::new();
    for character in sanitized.chars() {
        if in_tag {
            if character == '>' {
                let normalized = tag
                    .trim()
                    .to_ascii_lowercase()
                    .trim_end_matches('/')
                    .trim()
                    .split_whitespace()
                    .next()
                    .unwrap_or_default()
                    .to_string();
                if matches!(
                    normalized.as_str(),
                    "br" | "/p" | "/div" | "/li" | "/blockquote" | "/h1" | "/h2" | "/h3"
                ) {
                    if !output.ends_with('\n') {
                        output.push('\n');
                    }
                }
                in_tag = false;
                tag.clear();
            } else {
                tag.push(character);
            }
        } else if character == '<' {
            in_tag = true;
            tag.clear();
        } else {
            output.push(character);
        }
    }
    decode_html_entities(output.trim())
}

fn strip_style_blocks(input: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(relative_start) = lower[cursor..].find("<style") {
        let start = cursor + relative_start;
        let is_style_tag = lower
            .get(start + 6..)
            .and_then(|rest| rest.chars().next())
            .is_none_or(|character| character.is_ascii_whitespace() || character == '>');
        if !is_style_tag {
            let after_name = start + 6;
            output.push_str(&input[cursor..after_name]);
            cursor = after_name;
            continue;
        }

        output.push_str(&input[cursor..start]);
        let Some(open_end) = find_tag_end(input, start) else {
            break;
        };
        let Some(relative_close) = lower[open_end + 1..].find("</style") else {
            break;
        };
        let close_start = open_end + 1 + relative_close;
        let Some(close_end) = find_tag_end(input, close_start) else {
            break;
        };
        cursor = close_end + 1;
    }

    output.push_str(&input[cursor..]);
    output
}

fn decode_html_entities(value: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let Some(relative_start) = value[cursor..].find('&') else {
            result.push_str(&value[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        result.push_str(&value[cursor..start]);
        let Some(relative_end) = value[start + 1..].find(';') else {
            result.push_str(&value[start..]);
            break;
        };
        let end = start + 1 + relative_end;
        let entity = &value[start + 1..end];
        if let Some(decoded) = decode_html_entity(entity) {
            result.push(decoded);
            cursor = end + 1;
        } else {
            result.push_str(&value[start..=end]);
            cursor = end + 1;
        }
    }
    result.trim().to_string()
}

fn decode_html_entity(entity: &str) -> Option<char> {
    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
    }
    if let Some(decimal) = entity.strip_prefix('#') {
        return decimal.parse::<u32>().ok().and_then(char::from_u32);
    }
    match entity {
        "nbsp" => Some(' '),
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" => Some('\''),
        "auml" => Some('ä'),
        "ouml" => Some('ö'),
        "uuml" => Some('ü'),
        "Auml" => Some('Ä'),
        "Ouml" => Some('Ö'),
        "Uuml" => Some('Ü'),
        "szlig" => Some('ß'),
        "eacute" => Some('é'),
        "egrave" => Some('è'),
        "aring" => Some('å'),
        "copy" => Some('©'),
        "reg" => Some('®'),
        "ndash" => Some('–'),
        "mdash" => Some('—'),
        "hellip" => Some('…'),
        "laquo" => Some('«'),
        "raquo" => Some('»'),
        _ => None,
    }
}

fn find_tag_end(input: &str, start: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut quote = None;
    for index in start + 1..bytes.len() {
        let character = bytes[index] as char;
        match quote {
            Some(expected) if character == expected => quote = None,
            Some(_) => {}
            None if character == '\'' || character == '"' => quote = Some(character),
            None if character == '>' => return Some(index),
            None => {}
        }
    }
    None
}

fn find_block_end(input: &str, start: usize, tag_name: &str) -> Option<usize> {
    let lower = input[start..].to_ascii_lowercase();
    let closing = format!("</{tag_name}");
    let relative = lower.find(&closing)?;
    let absolute = start + relative;
    find_tag_end(input, absolute).map(|end| end + 1)
}

fn tag_name(raw_tag: &str) -> Option<(bool, String)> {
    let mut content = raw_tag.strip_prefix('<')?.strip_suffix('>')?.trim();
    let closing = content.starts_with('/');
    if closing {
        content = content[1..].trim_start();
    }
    if content.starts_with('!') || content.starts_with('?') {
        return None;
    }
    let name_end = content
        .char_indices()
        .find(|(_, character)| !character.is_ascii_alphanumeric())
        .map(|(index, _)| index)
        .unwrap_or(content.len());
    if name_end == 0 {
        return None;
    }
    Some((closing, content[..name_end].to_ascii_lowercase()))
}

fn sanitize_tag(raw_tag: &str, closing: bool, tag_name: &str) -> Option<String> {
    if !ALLOWED_TAGS.contains(&tag_name) {
        return None;
    }
    if closing {
        return Some(format!("</{tag_name}>"));
    }

    let content = raw_tag.strip_prefix('<')?.strip_suffix('>')?.trim();
    let name_end = content
        .char_indices()
        .find(|(_, character)| !character.is_ascii_alphanumeric())
        .map(|(index, _)| index)
        .unwrap_or(content.len());
    let mut cursor = name_end;
    let mut attributes = Vec::new();
    let bytes = content.as_bytes();

    while cursor < content.len() {
        while cursor < content.len()
            && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b'/')
        {
            cursor += 1;
        }
        if cursor >= content.len() {
            break;
        }
        let attribute_start = cursor;
        while cursor < content.len()
            && !bytes[cursor].is_ascii_whitespace()
            && bytes[cursor] != b'='
            && bytes[cursor] != b'/'
        {
            cursor += 1;
        }
        if attribute_start == cursor {
            cursor += 1;
            continue;
        }
        let attribute_name = content[attribute_start..cursor].to_ascii_lowercase();
        while cursor < content.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }

        let mut value = None;
        if cursor < content.len() && bytes[cursor] == b'=' {
            cursor += 1;
            while cursor < content.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < content.len() && (bytes[cursor] == b'\'' || bytes[cursor] == b'"') {
                let quote = bytes[cursor];
                cursor += 1;
                let value_start = cursor;
                while cursor < content.len() && bytes[cursor] != quote {
                    cursor += 1;
                }
                value = Some(content[value_start..cursor].to_string());
                if cursor < content.len() {
                    cursor += 1;
                }
            } else {
                let value_start = cursor;
                while cursor < content.len()
                    && !bytes[cursor].is_ascii_whitespace()
                    && bytes[cursor] != b'/'
                {
                    cursor += 1;
                }
                value = Some(content[value_start..cursor].to_string());
            }
        }

        if attribute_name == "style" {
            if let Some(value) = value.as_deref().and_then(sanitize_css) {
                attributes.push(format!(" style=\"{}\"", escape_attribute(&value)));
            }
            continue;
        }
        if is_blocked_attribute(&attribute_name) {
            continue;
        }
        if let Some(value) = value.as_deref() {
            if is_uri_attribute(&attribute_name) && !is_safe_uri(value, tag_name, &attribute_name) {
                continue;
            }
            attributes.push(format!(
                " {}=\"{}\"",
                attribute_name,
                escape_attribute(value)
            ));
        } else {
            attributes.push(format!(" {attribute_name}"));
        }
    }

    let self_closing = content.trim_end().ends_with('/');
    let tag_suffix = if self_closing || matches!(tag_name, "br" | "hr" | "img" | "col") {
        "/>"
    } else {
        ">"
    };
    Some(format!("<{tag_name}{}{tag_suffix}", attributes.concat()))
}

fn find_style_block_end(input: &str, start: usize) -> Option<(&str, usize)> {
    let lower = input[start..].to_ascii_lowercase();
    let relative_close = lower.find("</style")?;
    let close_start = start + relative_close;
    let close_end = find_tag_end(input, close_start)?;
    Some((&input[start..close_start], close_end + 1))
}

fn sanitize_css(value: &str) -> Option<String> {
    // Tracking comments in mail stylesheets sometimes contain URL-like
    // examples. Remove comments before checking declarations so one harmless
    // comment cannot cause the adjacent, useful layout rule to disappear.
    let without_comments = strip_css_comments(value);
    let trimmed = without_comments.trim();
    if trimmed.is_empty() {
        return None;
    }
    let blocked_tokens = [
        "url(",
        "@import",
        "@font-face",
        "@namespace",
        "expression(",
        "javascript:",
        "vbscript:",
        "behavior:",
        "-moz-binding",
        "-webkit-binding",
    ];

    // Keep safe rules even when a mail contains one unrelated tracking rule.
    // This is intentionally a small rule splitter, not a CSS parser: the
    // surrounding document sanitizer already removes active HTML, while the
    // token check prevents resource loads and browser-specific execution.
    let safe = trimmed
        .split_inclusive('}')
        .filter(|part| {
            let lower = part.to_ascii_lowercase();
            !blocked_tokens.iter().any(|token| lower.contains(token))
        })
        .collect::<String>();
    if safe.trim().is_empty() {
        return None;
    }
    Some(safe)
}

fn strip_css_comments(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let Some(relative_start) = value[cursor..].find("/*") else {
            output.push_str(&value[cursor..]);
            break;
        };
        let start = cursor + relative_start;
        output.push_str(&value[cursor..start]);
        let Some(relative_end) = value[start + 2..].find("*/") else {
            break;
        };
        cursor = start + 2 + relative_end + 2;
    }
    output
}

fn image_source(raw_tag: &str) -> Option<String> {
    let content = raw_tag.strip_prefix('<')?.strip_suffix('>')?.trim();
    let name_end = content
        .char_indices()
        .find(|(_, character)| !character.is_ascii_alphanumeric())
        .map(|(index, _)| index)
        .unwrap_or(content.len());
    let bytes = content.as_bytes();
    let mut cursor = name_end;

    while cursor < content.len() {
        while cursor < content.len()
            && (bytes[cursor].is_ascii_whitespace() || bytes[cursor] == b'/')
        {
            cursor += 1;
        }
        if cursor >= content.len() {
            break;
        }
        let attribute_start = cursor;
        while cursor < content.len()
            && !bytes[cursor].is_ascii_whitespace()
            && bytes[cursor] != b'='
            && bytes[cursor] != b'/'
        {
            cursor += 1;
        }
        let attribute_name = content[attribute_start..cursor].to_ascii_lowercase();
        while cursor < content.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= content.len() || bytes[cursor] != b'=' {
            continue;
        }
        cursor += 1;
        while cursor < content.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        let value = if cursor < content.len() && (bytes[cursor] == b'\'' || bytes[cursor] == b'"') {
            let quote = bytes[cursor];
            cursor += 1;
            let value_start = cursor;
            while cursor < content.len() && bytes[cursor] != quote {
                cursor += 1;
            }
            let value = content[value_start..cursor].to_string();
            if cursor < content.len() {
                cursor += 1;
            }
            value
        } else {
            let value_start = cursor;
            while cursor < content.len()
                && !bytes[cursor].is_ascii_whitespace()
                && bytes[cursor] != b'/'
            {
                cursor += 1;
            }
            content[value_start..cursor].to_string()
        };
        if attribute_name == "src" {
            return Some(value);
        }
    }

    None
}

fn is_remote_image_uri(value: &str) -> bool {
    let normalized = normalize_uri_prefix(value);
    normalized.starts_with("http://")
        || normalized.starts_with("https://")
        || normalized.starts_with("//")
}

fn is_blocked_attribute(attribute_name: &str) -> bool {
    attribute_name.starts_with("on")
        || matches!(
            attribute_name,
            "style" | "srcdoc" | "srcset" | "background" | "formaction" | "ping" | "integrity"
        )
}

fn is_uri_attribute(attribute_name: &str) -> bool {
    matches!(attribute_name, "href" | "src" | "action" | "xlink:href")
}

fn is_safe_uri(value: &str, tag_name: &str, attribute_name: &str) -> bool {
    let value = value.trim();
    let lower = normalize_uri_prefix(value);
    if lower.starts_with("javascript:")
        || lower.starts_with("vbscript:")
        || lower.starts_with("file:")
        || (lower.starts_with("data:") && !is_allowed_data_image(&lower, tag_name, attribute_name))
    {
        return false;
    }
    if lower.starts_with("data:") {
        return true;
    }

    let Some((scheme, _)) = lower.split_once(':') else {
        return true;
    };
    if scheme.is_empty() {
        return true;
    }

    match scheme {
        "http" | "https" => true,
        "mailto" | "tel" => tag_name == "a" && attribute_name == "href",
        "cid" => tag_name == "img" && matches!(attribute_name, "src" | "xlink:href"),
        _ => false,
    }
}

fn is_allowed_data_image(value: &str, tag_name: &str, attribute_name: &str) -> bool {
    if tag_name != "img" || !matches!(attribute_name, "src" | "xlink:href") {
        return false;
    }
    [
        "data:image/bmp",
        "data:image/gif",
        "data:image/jpeg",
        "data:image/png",
        "data:image/webp",
        "data:image/x-icon",
    ]
    .iter()
    .any(|prefix| value.starts_with(prefix))
}

fn normalize_uri_prefix(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < value.len() {
        let character = value[cursor..].chars().next().unwrap_or_default();
        if character.is_ascii_whitespace() || character.is_control() {
            cursor += character.len_utf8();
            continue;
        }
        if character == '&' {
            if let Some(relative_end) = value[cursor..].find(';') {
                let entity_end = cursor + relative_end + 1;
                if let Some(decoded) = decode_uri_entity(&value[cursor + 1..entity_end - 1]) {
                    normalized.push(decoded.to_ascii_lowercase());
                    cursor = entity_end;
                    continue;
                }
            }
        }
        normalized.push(character.to_ascii_lowercase());
        cursor += character.len_utf8();
    }
    if normalized.len() > 128 {
        normalized.truncate(128);
    }
    normalized
}

fn decode_uri_entity(entity: &str) -> Option<char> {
    if entity.eq_ignore_ascii_case("colon") {
        return Some(':');
    }
    if let Some(hex) = entity
        .strip_prefix("#x")
        .or_else(|| entity.strip_prefix("#X"))
    {
        return u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
    }
    entity
        .strip_prefix('#')
        .and_then(|decimal| decimal.parse::<u32>().ok())
        .and_then(char::from_u32)
}

fn escape_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_active_content_and_event_handlers() {
        let sanitized = sanitize_html(
            r#"<div onclick="alert(1)"><script>alert(2)</script><style>body{background:url(https://tracker)}</style><p>Hallo</p><iframe src="https://evil.example"></iframe></div>"#,
        );
        assert!(sanitized.contains("<div>"));
        assert!(sanitized.contains("<p>Hallo</p>"));
        assert!(!sanitized.contains("alert"));
        assert!(!sanitized.contains("tracker"));
        assert!(!sanitized.contains("iframe"));
        assert!(!sanitized.contains("onclick"));
    }

    #[test]
    fn rejects_dangerous_uris_but_keeps_safe_links_and_images() {
        let sanitized = sanitize_html(
            r#"<a href="javascript:alert(1)">bad</a><a href="jav&#x61;script:alert(1)">encoded</a><a href="ftp://example.org">unsupported</a><a href="mailto:person@example.org">mail</a><a href="https://example.org">good</a><img src="https://cdn.example/image.png" onerror="alert(1)"><img src="data:text/html,<script>bad</script>"><img src="data:image/svg+xml;base64,PHN2Zz4="><img src="data:image/png;base64,AAAA">"#,
        );
        assert!(sanitized.contains("<a>bad</a>"));
        assert!(sanitized.contains("<a>encoded</a>"));
        assert!(sanitized.contains("<a>unsupported</a>"));
        assert!(sanitized.contains("href=\"mailto:person@example.org\""));
        assert!(sanitized.contains("href=\"https://example.org\""));
        assert!(sanitized.contains("src=\"https://cdn.example/image.png\""));
        assert!(sanitized.contains("src=\"data:image/png;base64,AAAA\""));
        assert!(!sanitized.contains("javascript:"));
        assert!(!sanitized.contains("data:text/html"));
        assert!(!sanitized.contains("data:image/svg+xml"));
        assert!(!sanitized.contains("onerror"));
    }

    #[test]
    fn blocks_remote_images_but_keeps_inline_images() {
        let sanitized = sanitize_html(
            r#"<p><img src="https://cdn.example/remote.png"><img src="cid:logo@example.org"><img src="data:image/png;base64,AAAA"></p>"#,
        );
        let blocked = block_remote_images(&sanitized);
        assert!(!blocked.contains("https://cdn.example/remote.png"));
        assert!(blocked.contains("cid:logo@example.org"));
        assert!(blocked.contains("data:image/png;base64,AAAA"));
    }

    #[test]
    fn preserves_safe_mail_css_but_drops_resource_loading_css() {
        let sanitized = sanitize_html(
            r#"<style>.mail { color: #333; table-layout: fixed; } .unsafe { background: url(https://tracker.example/pixel); }</style><p class="mail" style="font-weight: bold">Hallo</p>"#,
        );
        assert!(sanitized.contains("<style>.mail { color: #333; table-layout: fixed; }"));
        assert!(!sanitized.contains("url("));
        assert!(sanitized.contains("style=\"font-weight: bold\""));
    }

    #[test]
    fn css_comments_do_not_hide_following_safe_rules() {
        let sanitized = sanitize_html(
            r#"<style>/* tracking example: url(https://tracker.example/pixel) */ .mail { color: #333; }</style><p class="mail">Hallo</p>"#,
        );

        assert!(sanitized.contains(".mail { color: #333; }"));
        assert!(!sanitized.contains("tracker.example"));
    }

    #[test]
    fn keeps_plain_text_and_handles_unclosed_tags_without_panicking() {
        assert_eq!(sanitize_html("plain text"), "plain text");
        let sanitized = sanitize_html("<p>Text ohne Ende <script>secret");
        assert!(sanitized.contains("<p>Text ohne Ende "));
        assert!(!sanitized.contains("secret"));
    }

    #[test]
    fn does_not_drop_mail_body_after_void_blocked_tags() {
        let sanitized = sanitize_html(
            r#"<html><head><meta charset="utf-8"><link rel="stylesheet" href="https://evil.example/mail.css"></head><body><p>Nachrichteninhalt</p></body></html>"#,
        );
        assert!(sanitized.contains("<p>Nachrichteninhalt</p>"));
        assert!(!sanitized.contains("meta"));
        assert!(!sanitized.contains("stylesheet"));
    }

    #[test]
    fn plain_text_search_follows_visible_html_boundaries() {
        assert_eq!(
            html_to_plain_text("<p>Quarterly</p><p>report<br>ready</p>"),
            "Quarterly\nreport\nready"
        );
    }

    #[test]
    fn plain_text_omits_preserved_mail_css() {
        assert_eq!(
            html_to_plain_text("<style>.mail { color: red; }</style><p class=\"mail\">Hallo</p>"),
            "Hallo"
        );
    }

    #[test]
    fn plain_text_decodes_named_and_numeric_html_entities() {
        assert_eq!(
            html_to_plain_text("<p>M&uuml;ller &amp; Team &#169; &#x1F4C1; &mdash; fertig</p>"),
            "Müller & Team © 📁 — fertig"
        );
    }
}
