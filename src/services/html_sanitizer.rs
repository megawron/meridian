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
    "applet", "audio", "base", "canvas", "embed", "form", "iframe", "link", "math", "meta",
    "object", "portal", "script", "source", "style", "svg", "template", "track", "video",
];

const ALLOWED_CSS_AT_RULES: &[&str] = &[
    "container",
    "keyframes",
    "layer",
    "media",
    "page",
    "supports",
    "-moz-keyframes",
    "-webkit-keyframes",
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
                    if let Some(css) = sanitize_stylesheet(css) {
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
                "applet"
                    | "audio"
                    | "canvas"
                    | "form"
                    | "iframe"
                    | "math"
                    | "object"
                    | "portal"
                    | "script"
                    | "source"
                    | "style"
                    | "svg"
                    | "template"
                    | "track"
                    | "video"
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
    sanitized_html_to_plain_text(&sanitize_html(value))
}

/// Convert already-sanitized mail markup to visible text without running the
/// sanitizer a second time. Callers must only use this after `sanitize_html`.
pub(crate) fn sanitized_html_to_plain_text(value: &str) -> String {
    let sanitized = strip_style_blocks(value);
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
            cursor = input.len();
            break;
        };
        let Some((_, after_block)) = find_style_block_end(input, open_end + 1) else {
            cursor = input.len();
            break;
        };
        cursor = after_block;
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
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(&closing) {
        let relative = cursor + relative;
        let after_name = relative + closing.len();
        let is_boundary = lower
            .as_bytes()
            .get(after_name)
            .is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'>');
        if is_boundary {
            let absolute = start + relative;
            return find_tag_end(input, absolute).map(|end| end + 1);
        }
        cursor = after_name;
    }
    None
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

#[derive(Debug, PartialEq, Eq)]
struct ParsedAttribute {
    name: String,
    value: Option<String>,
}

fn sanitize_tag(raw_tag: &str, closing: bool, tag_name: &str) -> Option<String> {
    if !ALLOWED_TAGS.contains(&tag_name) {
        return None;
    }
    if closing {
        return Some(format!("</{tag_name}>"));
    }

    let (parsed_attributes, self_closing) = parse_attributes(raw_tag)?;
    let mut attributes = Vec::new();
    for attribute in parsed_attributes {
        let attribute_name = attribute.name;
        let value = attribute.value;

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
            if attribute_name == "rel" {
                let safe_rel = sanitize_link_relations(value);
                if safe_rel.is_empty() {
                    continue;
                }
                attributes.push(format!(" rel=\"{}\"", escape_attribute(&safe_rel)));
                continue;
            }
            if is_uri_attribute(&attribute_name) && !is_safe_uri(value, tag_name, &attribute_name) {
                continue;
            }
            attributes.push(format!(" {attribute_name}=\"{}\"", escape_attribute(value)));
        } else {
            attributes.push(format!(" {attribute_name}"));
        }
    }

    let tag_suffix = if self_closing || matches!(tag_name, "br" | "hr" | "img" | "col") {
        "/>"
    } else {
        ">"
    };
    Some(format!("<{tag_name}{}{tag_suffix}", attributes.concat()))
}

fn parse_attributes(raw_tag: &str) -> Option<(Vec<ParsedAttribute>, bool)> {
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
                // A slash is valid inside an unquoted HTML attribute value.
                // Stopping at it turns `https://...` into `https:` and can
                // also make the trailing path look like another attribute.
                let value_start = cursor;
                while cursor < content.len() && !is_unquoted_attribute_end(bytes[cursor]) {
                    cursor += 1;
                }
                value = Some(content[value_start..cursor].to_string());
            }
        }

        attributes.push(ParsedAttribute {
            name: attribute_name,
            value,
        });
    }

    Some((attributes, content.trim_end().ends_with('/')))
}

fn is_unquoted_attribute_end(byte: u8) -> bool {
    byte.is_ascii_whitespace() || matches!(byte, b'"' | b'\'' | b'=' | b'<' | b'`')
}

fn find_style_block_end(input: &str, start: usize) -> Option<(&str, usize)> {
    let lower = input[start..].to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative_close) = lower[cursor..].find("</style") {
        let relative_close = cursor + relative_close;
        let after_name = relative_close + "</style".len();
        let is_boundary = lower
            .as_bytes()
            .get(after_name)
            .is_none_or(|byte| byte.is_ascii_whitespace() || *byte == b'>');
        if is_boundary {
            let close_start = start + relative_close;
            let close_end = find_tag_end(input, close_start)?;
            return Some((&input[start..close_start], close_end + 1));
        }
        cursor = after_name;
    }
    None
}

fn sanitize_css(value: &str) -> Option<String> {
    let trimmed = strip_css_comments(value);
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('@') || has_top_level_css_brace(trimmed) {
        sanitize_stylesheet(trimmed)
    } else {
        sanitize_inline_css(trimmed)
    }
}

fn sanitize_stylesheet(value: &str) -> Option<String> {
    let without_comments = strip_css_comments(value);
    let sanitized = sanitize_css_rules(&without_comments);
    (!sanitized.trim().is_empty()).then_some(sanitized)
}

fn sanitize_inline_css(value: &str) -> Option<String> {
    let without_comments = strip_css_comments(value);
    let declarations = sanitize_css_declarations(&without_comments)?;
    let declarations = declarations.trim().trim_end_matches(';').trim();
    (!declarations.is_empty()).then_some(declarations.to_string())
}

fn sanitize_css_rules(value: &str) -> String {
    let mut output = String::new();
    let mut cursor = 0;

    while cursor < value.len() {
        while cursor < value.len() && value.as_bytes()[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if cursor >= value.len() || value.as_bytes()[cursor] == b'}' {
            break;
        }

        let Some((delimiter, delimiter_position)) = find_css_delimiter(value, cursor) else {
            break;
        };
        let prelude = value[cursor..delimiter_position].trim();
        if prelude.is_empty() {
            cursor = delimiter_position + 1;
            continue;
        }

        match delimiter {
            b';' => {
                // Standalone at-rules such as @import and @charset are
                // intentionally discarded. They cannot contribute layout,
                // and some of them load a remote resource.
                cursor = delimiter_position + 1;
            }
            b'{' => {
                let Some(close) = find_matching_css_brace(value, delimiter_position) else {
                    break;
                };
                if let Some(rule) =
                    sanitize_css_block(prelude, &value[delimiter_position + 1..close])
                {
                    output.push_str(&rule);
                }
                cursor = close + 1;
            }
            b'}' => break,
            _ => break,
        }
    }

    output
}

fn sanitize_css_block(prelude: &str, body: &str) -> Option<String> {
    if css_contains_blocked_construct(prelude) {
        return None;
    }

    if prelude.starts_with('@') {
        let at_rule = css_at_rule_name(prelude)?;
        if !ALLOWED_CSS_AT_RULES.contains(&at_rule.as_str()) {
            return None;
        }

        let sanitized_body = if at_rule == "page" {
            sanitize_css_declarations(body)?
        } else {
            let rules = sanitize_css_rules(body);
            if rules.trim().is_empty() {
                return None;
            }
            rules
        };
        return Some(format!(
            "{} {{ {} }}",
            prelude.trim(),
            sanitized_body.trim()
        ));
    }

    let declarations = sanitize_css_declarations(body)?;
    Some(format!("{} {{ {} }}", prelude.trim(), declarations.trim()))
}

fn sanitize_css_declarations(value: &str) -> Option<String> {
    let mut output = String::new();
    let mut start = 0;

    for end in css_declaration_boundaries(value) {
        let declaration = value[start..end].trim();
        start = end + 1;
        let Some((property, property_value)) = split_css_declaration(declaration) else {
            continue;
        };
        let property = property.trim();
        let property_value = property_value.trim();
        if !is_safe_css_property(property)
            || property_value.is_empty()
            || css_contains_blocked_construct(property_value)
        {
            continue;
        }
        output.push_str(property);
        output.push_str(": ");
        output.push_str(property_value);
        output.push_str("; ");
    }

    let declaration = value[start..].trim();
    if let Some((property, property_value)) = split_css_declaration(declaration) {
        let property = property.trim();
        let property_value = property_value.trim();
        if is_safe_css_property(property)
            && !property_value.is_empty()
            && !css_contains_blocked_construct(property_value)
        {
            output.push_str(property);
            output.push_str(": ");
            output.push_str(property_value);
            output.push_str("; ");
        }
    }

    (!output.trim().is_empty()).then_some(output)
}

fn css_declaration_boundaries(value: &str) -> Vec<usize> {
    let bytes = value.as_bytes();
    let mut boundaries = Vec::new();
    let mut cursor = 0;
    let mut quote = None;
    let mut parentheses = 0usize;
    let mut brackets = 0usize;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(expected) = quote {
            if byte == b'\\' {
                cursor = css_escape_end(value, cursor);
                continue;
            }
            if byte == expected {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if byte == b'\\' {
            cursor = css_escape_end(value, cursor);
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' => parentheses = parentheses.saturating_add(1),
            b')' => parentheses = parentheses.saturating_sub(1),
            b'[' => brackets = brackets.saturating_add(1),
            b']' => brackets = brackets.saturating_sub(1),
            b';' if parentheses == 0 && brackets == 0 => boundaries.push(cursor),
            _ => {}
        }
        cursor += 1;
    }
    boundaries
}

fn split_css_declaration(value: &str) -> Option<(&str, &str)> {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    let mut quote = None;
    let mut parentheses = 0usize;
    let mut brackets = 0usize;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(expected) = quote {
            if byte == b'\\' {
                cursor = css_escape_end(value, cursor);
                continue;
            }
            if byte == expected {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if byte == b'\\' {
            cursor = css_escape_end(value, cursor);
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' => parentheses = parentheses.saturating_add(1),
            b')' => parentheses = parentheses.saturating_sub(1),
            b'[' => brackets = brackets.saturating_add(1),
            b']' => brackets = brackets.saturating_sub(1),
            b':' if parentheses == 0 && brackets == 0 => {
                return Some((&value[..cursor], &value[cursor + 1..]));
            }
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn is_safe_css_property(property: &str) -> bool {
    let property = property.trim();
    if property.is_empty() || property.contains(char::is_whitespace) {
        return false;
    }
    let normalized = normalize_css_for_policy(property);
    if normalized.is_empty()
        || normalized.contains('{')
        || normalized.contains('}')
        || normalized.contains(';')
    {
        return false;
    }
    ![
        "behavior",
        "-moz-binding",
        "-webkit-binding",
        "-ms-behavior",
        "-o-link",
        "-moz-link",
        "-webkit-link",
    ]
    .iter()
    .any(|blocked| normalized == *blocked)
}

fn has_top_level_css_brace(value: &str) -> bool {
    let bytes = value.as_bytes();
    let mut cursor = 0;
    let mut quote = None;
    let mut parentheses = 0usize;
    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(expected) = quote {
            if byte == b'\\' {
                cursor = css_escape_end(value, cursor);
                continue;
            }
            if byte == expected {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if byte == b'\\' {
            cursor = css_escape_end(value, cursor);
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' => parentheses = parentheses.saturating_add(1),
            b')' => parentheses = parentheses.saturating_sub(1),
            b'{' if parentheses == 0 => return true,
            _ => {}
        }
        cursor += 1;
    }
    false
}

fn find_css_delimiter(value: &str, start: usize) -> Option<(u8, usize)> {
    let bytes = value.as_bytes();
    let mut cursor = start;
    let mut quote = None;
    let mut parentheses = 0usize;
    let mut brackets = 0usize;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(expected) = quote {
            if byte == b'\\' {
                cursor = css_escape_end(value, cursor);
                continue;
            }
            if byte == expected {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if byte == b'\\' {
            cursor = css_escape_end(value, cursor);
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'(' => parentheses = parentheses.saturating_add(1),
            b')' => parentheses = parentheses.saturating_sub(1),
            b'[' => brackets = brackets.saturating_add(1),
            b']' => brackets = brackets.saturating_sub(1),
            b'{' | b'}' | b';' if parentheses == 0 && brackets == 0 => {
                return Some((byte, cursor));
            }
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn find_matching_css_brace(value: &str, opening: usize) -> Option<usize> {
    let bytes = value.as_bytes();
    let mut cursor = opening + 1;
    let mut depth = 1usize;
    let mut quote = None;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(expected) = quote {
            if byte == b'\\' {
                cursor = css_escape_end(value, cursor);
                continue;
            }
            if byte == expected {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if byte == b'\\' {
            cursor = css_escape_end(value, cursor);
            continue;
        }
        match byte {
            b'\'' | b'"' => quote = Some(byte),
            b'{' => depth = depth.saturating_add(1),
            b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(cursor);
                }
            }
            _ => {}
        }
        cursor += 1;
    }
    None
}

fn css_at_rule_name(prelude: &str) -> Option<String> {
    let mut cursor = 1;
    let mut name = String::new();
    while cursor < prelude.len() {
        let byte = prelude.as_bytes()[cursor];
        if byte == b'\\' {
            let (decoded, next) = decode_css_escape(prelude, cursor);
            if !is_css_identifier_character(decoded) {
                break;
            }
            name.push(decoded.to_ascii_lowercase());
            cursor = next;
            continue;
        }
        let character = byte as char;
        if !is_css_identifier_character(character) {
            break;
        }
        name.push(character.to_ascii_lowercase());
        cursor += 1;
    }
    (!name.is_empty()).then_some(name)
}

fn css_contains_blocked_construct(value: &str) -> bool {
    let normalized = normalize_css_for_policy(value);
    [
        "@font-face",
        "@import",
        "@namespace",
        "behavior:",
        "-moz-binding",
        "-ms-behavior",
        "-o-link",
        "-moz-link",
        "-webkit-binding",
        "-webkit-link",
        "javascript:",
        "vbscript:",
        "progid:",
    ]
    .iter()
    .any(|token| normalized.contains(token))
        || [
            "url",
            "expression",
            "image-set",
            "-webkit-image-set",
            "cross-fade",
            "paint",
        ]
        .iter()
        .any(|function| css_function_present(&normalized, function))
}

fn css_function_present(value: &str, function: &str) -> bool {
    for (position, _) in value.match_indices(function) {
        let bytes = value.as_bytes();
        let before_is_identifier = position > 0 && is_css_identifier_byte(bytes[position - 1]);
        if before_is_identifier {
            continue;
        }
        let mut cursor = position + function.len();
        while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
            cursor += 1;
        }
        if bytes.get(cursor) == Some(&b'(') {
            return true;
        }
    }
    false
}

fn normalize_css_for_policy(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut cursor = 0;
    let bytes = value.as_bytes();
    let mut quote = None;

    while cursor < bytes.len() {
        let byte = bytes[cursor];
        if let Some(expected) = quote {
            if byte == b'\\' {
                cursor = css_escape_end(value, cursor);
                continue;
            }
            if byte == expected {
                quote = None;
            }
            cursor += 1;
            continue;
        }
        if byte == b'\'' || byte == b'"' {
            quote = Some(byte);
            normalized.push(' ');
            cursor += 1;
            continue;
        }
        if byte == b'\\' {
            let (decoded, next) = decode_css_escape(value, cursor);
            normalized.push(decoded.to_ascii_lowercase());
            cursor = next;
            continue;
        }
        let character = value[cursor..].chars().next().unwrap_or_default();
        normalized.push(character.to_ascii_lowercase());
        cursor += character.len_utf8();
    }

    normalized
}

fn is_css_identifier_character(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '-' | '_')
}

fn is_css_identifier_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_') || byte >= 0x80
}

fn css_escape_end(value: &str, start: usize) -> usize {
    decode_css_escape(value, start).1
}

fn decode_css_escape(value: &str, start: usize) -> (char, usize) {
    let bytes = value.as_bytes();
    if bytes.get(start) != Some(&b'\\') {
        return (value[start..].chars().next().unwrap_or_default(), start + 1);
    }
    let Some(next) = bytes.get(start + 1).copied() else {
        return ('\\', start + 1);
    };
    if next.is_ascii_hexdigit() {
        let mut cursor = start + 1;
        let mut digits = 0;
        while cursor < bytes.len() && digits < 6 && bytes[cursor].is_ascii_hexdigit() {
            cursor += 1;
            digits += 1;
        }
        let hex = &value[start + 1..cursor];
        let decoded = u32::from_str_radix(hex, 16)
            .ok()
            .and_then(char::from_u32)
            .unwrap_or('\u{fffd}');
        if bytes
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            cursor += 1;
        }
        return (decoded, cursor);
    }
    let character = value[start + 1..].chars().next().unwrap_or('\\');
    (character, start + 1 + character.len_utf8())
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
    parse_attributes(raw_tag)?
        .0
        .into_iter()
        .find(|attribute| attribute.name == "src")
        .and_then(|attribute| attribute.value)
}

fn is_remote_image_uri(value: &str) -> bool {
    let normalized = normalize_uri_prefix(value);
    let percent_decoded = percent_decode_uri_prefix(&normalized);
    [normalized.as_str(), percent_decoded.as_str()]
        .iter()
        .any(|uri| {
            uri.starts_with("http://") || uri.starts_with("https://") || uri.starts_with("//")
        })
}

fn is_blocked_attribute(attribute_name: &str) -> bool {
    attribute_name.starts_with("on")
        || matches!(
            attribute_name,
            "action"
                | "archive"
                | "background"
                | "classid"
                | "codebase"
                | "data"
                | "dynsrc"
                | "formaction"
                | "imagesrcset"
                | "integrity"
                | "lowsrc"
                | "manifest"
                | "ping"
                | "poster"
                | "srcdoc"
                | "srcset"
                | "style"
                | "usemap"
                | "xml:base"
        )
}

fn sanitize_link_relations(value: &str) -> String {
    const BLOCKED_RELATIONS: &[&str] = &[
        "dns-prefetch",
        "import",
        "modulepreload",
        "preconnect",
        "prefetch",
        "prerender",
        "preload",
        "stylesheet",
    ];

    value
        .split_ascii_whitespace()
        .filter(|relation| {
            !BLOCKED_RELATIONS
                .iter()
                .any(|blocked| relation.eq_ignore_ascii_case(blocked.trim()))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_uri_attribute(attribute_name: &str) -> bool {
    matches!(attribute_name, "href" | "src" | "action" | "xlink:href")
}

fn is_safe_uri(value: &str, tag_name: &str, attribute_name: &str) -> bool {
    let is_allowed_target = match attribute_name {
        "href" => tag_name == "a",
        "src" | "xlink:href" => tag_name == "img",
        // Forms are blocked as containers, so keeping an action URI would
        // only create an unnecessary future bypass if another form-like tag
        // is ever admitted.
        "action" => false,
        _ => false,
    };
    if !is_allowed_target {
        return false;
    }

    let value = value.trim();
    let lower = normalize_uri_prefix(value);
    let percent_decoded = percent_decode_uri_prefix(&lower);
    if [lower.as_str(), percent_decoded.as_str()]
        .iter()
        .any(|uri| {
            uri.starts_with("javascript:")
                || uri.starts_with("vbscript:")
                || uri.starts_with("file:")
                || uri.starts_with("data:text/html")
                || uri.starts_with("data:image/svg")
        })
    {
        return false;
    }
    if lower.starts_with("data:") {
        return is_allowed_data_image(&lower, tag_name, attribute_name);
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
    let Some(image_data) = value.strip_prefix("data:image/") else {
        return false;
    };
    let mime_end = image_data.find([';', ',']).unwrap_or(image_data.len());
    matches!(
        &image_data[..mime_end],
        "avif" | "apng" | "bmp" | "gif" | "jpeg" | "png" | "webp" | "x-icon" | "vnd.microsoft.icon"
    )
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
                    if !decoded.is_ascii_whitespace() && !decoded.is_control() {
                        normalized.push(decoded.to_ascii_lowercase());
                    }
                    cursor = entity_end;
                    continue;
                }
            }
        }
        let lowered = character.to_ascii_lowercase();
        if normalized.len() + lowered.len_utf8() > 128 {
            break;
        }
        normalized.push(lowered);
        cursor += character.len_utf8();
    }
    normalized
}

fn percent_decode_uri_prefix(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut decoded = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < bytes.len() && decoded.len() < 128 {
        if bytes[cursor] == b'%' && cursor + 2 < bytes.len() {
            let high = (bytes[cursor + 1] as char).to_digit(16);
            let low = (bytes[cursor + 2] as char).to_digit(16);
            if let (Some(high), Some(low)) = (high, low) {
                if let Some(character) = char::from_u32(high * 16 + low) {
                    if !character.is_ascii_whitespace() && !character.is_control() {
                        decoded.push(character.to_ascii_lowercase());
                    }
                    cursor += 3;
                    continue;
                }
            }
        }
        let character = value[cursor..].chars().next().unwrap_or_default();
        if decoded.len() + character.len_utf8() > 128 {
            break;
        }
        decoded.push(character.to_ascii_lowercase());
        cursor += character.len_utf8();
    }
    decoded
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

    #[test]
    fn preserves_newsletter_and_office_layout_css_while_filtering_resources() {
        let sanitized = sanitize_html(
            r##"<style>
                @media screen and (max-width: 600px) {
                    .wrapper { width: 100% !important; background-color: #ffffff; }
                    .hero { color: #ffffff; font-family: Arial, sans-serif; background: #1f4e79; background-image: url('https://tracker.example/pixel'); }
                }
                .mso-table { mso-table-lspace: 0pt; mso-table-rspace: 0pt; border-collapse: collapse; }
                @import url(https://tracker.example/mail.css);
                @font-face { font-family: Tracking; src: url(https://tracker.example/font.woff2); }
            </style>
            <table class="wrapper mso-table" bgcolor="#ffffff"><tr><td style="color:#1f4e79; background-color: #ffffff; font-size: 16px; background-image: url(https://tracker.example/pixel)">Hallo</td></tr></table>"##,
        );

        assert!(sanitized.contains("@media screen and (max-width: 600px)"));
        assert!(sanitized.contains("width: 100% !important;"));
        assert!(sanitized.contains("background-color: #ffffff;"));
        assert!(sanitized.contains("font-family: Arial, sans-serif;"));
        assert!(sanitized.contains("mso-table-lspace: 0pt;"));
        assert!(sanitized.contains("border-collapse: collapse;"));
        assert!(sanitized.contains("bgcolor=\"#ffffff\""));
        assert!(sanitized.contains("color: #1f4e79"));
        assert!(!sanitized.contains("tracker.example"));
        assert!(!sanitized.contains("@import"));
        assert!(!sanitized.contains("@font-face"));
        assert!(!sanitized.contains("url("));
    }

    #[test]
    fn filters_only_unsafe_inline_declarations() {
        let sanitized = sanitize_html(
            r#"<p style="color: #222; background: #fff; font-size: 15px; line-height: 1.5; background-image: url(https://tracker.example/pixel);">Nachricht</p>"#,
        );

        assert!(sanitized.contains("color: #222"));
        assert!(sanitized.contains("background: #fff"));
        assert!(sanitized.contains("font-size: 15px"));
        assert!(sanitized.contains("line-height: 1.5"));
        assert!(!sanitized.contains("background-image"));
        assert!(!sanitized.contains("tracker.example"));
    }

    #[test]
    fn blocks_obfuscated_css_resource_and_execution_functions() {
        let sanitized = sanitize_html(
            r#"<style>
                .escaped { background: u\72l(https://tracker.example/pixel); color: red; }
                .spaced { background-image: url (https://tracker.example/pixel); font-weight: bold; }
                .expression { color: expression(alert(1)); font-style: italic; }
            </style>"#,
        );

        assert!(sanitized.contains("color: red;"));
        assert!(sanitized.contains("font-weight: bold;"));
        assert!(sanitized.contains("font-style: italic;"));
        assert!(!sanitized.contains("tracker.example"));
        assert!(!sanitized.contains("url"));
        assert!(!sanitized.contains("expression("));
    }

    #[test]
    fn parses_unquoted_image_urls_before_applying_remote_image_policy() {
        let sanitized = sanitize_html(
            r#"<table bgcolor=#ffffff><tr><td><img SRC=https://cdn.example/assets/pixel.png><img src=https%3A%2F%2Fcdn.example/encoded.png><img src=cid:logo@example.org></td></tr></table>"#,
        );
        let blocked = block_remote_images(&sanitized);

        assert!(sanitized.contains("src=\"https://cdn.example/assets/pixel.png\""));
        assert!(sanitized.contains("bgcolor=\"#ffffff\""));
        assert!(!blocked.contains("cdn.example"));
        assert!(blocked.contains("src=\"cid:logo@example.org\""));
    }

    #[test]
    fn rejects_entity_and_percent_obfuscated_dangerous_uris() {
        let sanitized = sanitize_html(
            r#"<a href="java&#x09;script:alert(1)">control</a><a href="jav&#x73;cript:alert(1)">entity</a><a href="javascript%3Aalert(1)">percent</a><a href="https://example.org">safe</a>"#,
        );

        assert_eq!(sanitized.matches("href=").count(), 1);
        assert!(sanitized.contains("href=\"https://example.org\""));
        assert!(!sanitized.contains("javascript"));
    }

    #[test]
    fn removes_remote_resource_hint_attributes_but_keeps_safe_link_relations() {
        let sanitized = sanitize_html(
            r#"<a rel="prefetch stylesheet noopener" href="https://example.org">Link</a><img lowsrc="https://tracker.example/old.gif" imagesrcset="https://tracker.example/retina.png"><p>Inhalt</p>"#,
        );

        assert!(sanitized.contains("rel=\"noopener\""));
        assert!(sanitized.contains("href=\"https://example.org\""));
        assert!(!sanitized.contains("prefetch"));
        assert!(!sanitized.contains("stylesheet"));
        assert!(!sanitized.contains("lowsrc"));
        assert!(!sanitized.contains("imagesrcset"));
    }
}
