// SPDX-License-Identifier: GPL-3.0-or-later
//
// Small, dependency-free sanitizer for HTML mail before it reaches WebKit.
//
// WebKit remains sandboxed and JavaScript-disabled, but untrusted mail is
// still reduced here so that event handlers, active embeds, CSS resource
// loads, and dangerous URI schemes do not enter the document at all.

const ALLOWED_TAGS: &[&str] = &[
    "a",
    "abbr",
    "address",
    "article",
    "b",
    "blockquote",
    "body",
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
    "picture",
    "pre",
    "s",
    "section",
    "small",
    "source",
    "span",
    "strike",
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
    "wbr",
];

const BLOCKED_TAGS: &[&str] = &[
    "applet", "audio", "base", "canvas", "embed", "form", "iframe", "link", "math", "meta",
    "noembed", "noscript", "object", "portal", "script", "style", "svg", "template", "title", "track",
    "video", "xml",
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
    sanitize_html_with_options(input, false)
}

/// Sanitize untrusted mail HTML with optional allowance of remote resources
/// (`srcset`, `background`, and CSS `url(...)`).
pub fn sanitize_html_with_options(input: &str, allow_remote_resources: bool) -> String {
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
                    if let Some(css) = sanitize_stylesheet(css, allow_remote_resources) {
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
                    | "noembed"
                    | "noscript"
                    | "object"
                    | "portal"
                    | "script"
                    | "style"
                    | "svg"
                    | "template"
                    | "title"
                    | "video"
                    | "xml"
            );
            if !closing && is_container {
                cursor = find_block_end(input, end + 1, &tag_name).unwrap_or(input.len());
            } else {
                cursor = end + 1;
            }
            continue;
        }

        if let Some(safe_tag) = sanitize_tag(raw_tag, closing, &tag_name, allow_remote_resources) {
            output.push_str(&safe_tag);
        }
        cursor = end + 1;
    }

    output
}

/// SVG Data URI representing a minimalist, Libadwaita-inspired vector placeholder icon.
/// Uses a continuous mountain ridgeline clipped to the inner frame and grouped opacity (0.55)
/// so line intersections/overlaps never darken and strokes never protrude past rounded corners.
pub const PLACEHOLDER_IMAGE_DATA_URI: &str =
    "data:image/svg+xml;base64,PHN2ZyB4bWxucz0iaHR0cDovL3d3dy53My5vcmcvMjAwMC9zdmciIHZpZXdCb3g9IjAgMCAzMiAzMiIgd2lkdGg9IjMyIiBoZWlnaHQ9IjMyIiBmaWxsPSJub25lIiBzdHJva2U9InJnYigxMzYsMTM2LDEzNikiIHN0cm9rZS13aWR0aD0iMS44IiBzdHJva2UtbGluZWNhcD0icm91bmQiIHN0cm9rZS1saW5lam9pbj0icm91bmQiPjxkZWZzPjxjbGlwUGF0aCBpZD0iZiI+PHJlY3QgeD0iNCIgeT0iNSIgd2lkdGg9IjI0IiBoZWlnaHQ9IjIyIiByeD0iNCIvPjwvY2xpcFBhdGg+PC9kZWZzPjxnIG9wYWNpdHk9IjAuNTUiPjxyZWN0IHg9IjQiIHk9IjUiIHdpZHRoPSIyNCIgaGVpZ2h0PSIyMiIgcng9IjQiLz48Y2lyY2xlIGN4PSIxMSIgY3k9IjExLjUiIHI9IjIuMiIgZmlsbD0icmdiKDEzNiwxMzYsMTM2KSIgc3Ryb2tlPSJub25lIi8+PHBhdGggZD0iTTQgMjIuNSBMMTIuNSAxNi41IEwxNiAyMCBMMjEgMTUgTDI4IDIyIiBjbGlwLXBhdGg9InVybCgjZikiLz48L2c+PC9zdmc+";

/// 1x1 Transparent GIF Data URI for invisible layout spacers.
pub const TRANSPARENT_PIXEL_DATA_URI: &str =
    "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///yH5BAEAAAAALAAAAAABAAEAAAIBRAA7";

fn parse_dimension(val: &str) -> Option<u32> {
    let trimmed = val.trim().trim_end_matches("px").trim();
    trimmed.parse::<u32>().ok()
}

fn extract_dimension_from_style(style: &str, prop: &str) -> Option<u32> {
    let style_lower = style.to_ascii_lowercase();
    for declaration in style_lower.split(';') {
        let mut parts = declaration.splitn(2, ':');
        if let (Some(k), Some(v)) = (parts.next(), parts.next()) {
            if k.trim() == prop {
                return parse_dimension(v);
            }
        }
    }
    None
}

fn get_tag_dimensions(raw_tag: &str) -> (Option<u32>, Option<u32>) {
    let width = image_attribute(raw_tag, "width")
        .and_then(|w| parse_dimension(&w))
        .or_else(|| {
            image_attribute(raw_tag, "style")
                .and_then(|s| extract_dimension_from_style(&s, "width"))
        });

    let height = image_attribute(raw_tag, "height")
        .and_then(|h| parse_dimension(&h))
        .or_else(|| {
            image_attribute(raw_tag, "style")
                .and_then(|s| extract_dimension_from_style(&s, "height"))
        });

    (width, height)
}

fn is_tracking_pixel(raw_tag: &str) -> bool {
    // 1. Hidden via style
    if let Some(style) = image_attribute(raw_tag, "style") {
        let style_lower = style.to_ascii_lowercase();
        if style_lower.contains("display:none")
            || style_lower.contains("display: none")
            || style_lower.contains("visibility:hidden")
            || style_lower.contains("visibility: hidden")
            || style_lower.contains("width:0")
            || style_lower.contains("width: 0")
            || style_lower.contains("height:0")
            || style_lower.contains("height: 0")
        {
            return true;
        }
    }

    // 2. Known tracking keywords in src
    if let Some(src) = image_attribute(raw_tag, "src") {
        let src_lower = src.to_ascii_lowercase();
        if src_lower.contains("track")
            || src_lower.contains("beacon")
            || src_lower.contains("open.php")
            || src_lower.contains("pixel.gif")
            || src_lower.contains("1x1")
            || src_lower.contains("tr.gif")
            || src_lower.contains("stat.php")
        {
            return true;
        }
    }

    let (width, height) = get_tag_dimensions(raw_tag);

    // 3. Dimension checks:
    // If either dimension is 0
    if width == Some(0) || height == Some(0) {
        return true;
    }

    // If both dimensions are specified and <= 3, it's a tracking pixel
    if let (Some(w), Some(h)) = (width, height) {
        if w <= 3 && h <= 3 {
            return true;
        }
    } else {
        // If only one dimension is specified and <= 1, it's a 1px tracking beacon
        if width.is_some_and(|w| w <= 1) || height.is_some_and(|h| h <= 1) {
            return true;
        }
    }

    false
}

fn is_spacer_image(raw_tag: &str) -> bool {
    // 1. Check URI / Filename for common spacer and blank image names
    if let Some(src) = image_attribute(raw_tag, "src") {
        let src_lower = src.to_ascii_lowercase();
        let filename = src_lower.split('?').next().unwrap_or(&src_lower);
        let name = filename.rsplit('/').next().unwrap_or(filename);
        if name.contains("spacer")
            || name.contains("blank")
            || name.contains("transparent")
            || name.contains("cleardot")
            || name.contains("shim")
            || name.contains("dot.gif")
            || name.contains("dot.png")
            || name.contains("empty")
            || name.contains("space.gif")
            || name.contains("trans.gif")
            || name.contains("trans.png")
            || name.contains("whitespace")
        {
            return true;
        }
    }

    // 2. Check class names
    if let Some(class) = image_attribute(raw_tag, "class") {
        let class_lower = class.to_ascii_lowercase();
        if class_lower.contains("spacer") || class_lower.contains("blank") {
            return true;
        }
    }

    // 3. Check alt text
    let alt = image_attribute(raw_tag, "alt");
    let has_meaningful_alt = alt.as_ref().is_some_and(|a| {
        let t = a.trim();
        !t.is_empty() && !t.eq_ignore_ascii_case("spacer") && !t.eq_ignore_ascii_case("blank")
    });

    let (width, height) = get_tag_dimensions(raw_tag);

    // 4. Dimension-based heuristics for layout spacers:
    // If either dimension is very small (<= 4px), it is a divider or spacer line
    if width.is_some_and(|w| w <= 4) || height.is_some_and(|h| h <= 4) {
        return true;
    }

    // 5. Narrow margin columns or table cell padders without meaningful alt text
    if !has_meaningful_alt {
        if let (Some(w), Some(h)) = (width, height) {
            // Small cell spacer (<= 20x20)
            if w <= 20 && h <= 20 {
                return true;
            }
            // Vertical margin spacers (e.g. width <= 20px, any height)
            if w <= 20 {
                return true;
            }
            // Horizontal divider spacers (e.g. height <= 20px, any width)
            if h <= 20 {
                return true;
            }
        }
    }

    // 6. role="presentation" or aria-hidden="true" with small dimensions
    if let Some(role) = image_attribute(raw_tag, "role") {
        if role.trim().eq_ignore_ascii_case("presentation") {
            if width.is_some_and(|w| w <= 32) || height.is_some_and(|h| h <= 32) {
                return true;
            }
        }
    }

    false
}

fn build_transparent_spacer_tag(raw_tag: &str) -> String {
    let (width, height) = get_tag_dimensions(raw_tag);
    let (parsed_attributes, self_closing) =
        parse_attributes(raw_tag).unwrap_or_else(|| (Vec::new(), false));
    let mut attributes = Vec::new();
    attributes.push(format!(" src=\"{TRANSPARENT_PIXEL_DATA_URI}\""));

    let mut existing_style = None;

    for attribute in parsed_attributes {
        let name_lower = attribute.name.to_ascii_lowercase();
        if matches!(
            name_lower.as_str(),
            "src" | "srcset" | "imagesrcset" | "xlink:href" | "data-meridian-placeholder"
        ) {
            continue;
        }
        if name_lower == "style" {
            existing_style = attribute.value;
            continue;
        }
        if let Some(val) = attribute.value {
            attributes.push(format!(" {}=\"{}\"", attribute.name, escape_attribute(&val)));
        } else {
            attributes.push(format!(" {}", attribute.name));
        }
    }

    // Build constrained style for layout spacers so they cannot balloon in height
    let mut style_parts = Vec::new();
    if let Some(ref style) = existing_style {
        let trimmed = style.trim().trim_end_matches(';').trim();
        if !trimmed.is_empty() {
            style_parts.push(trimmed.to_string());
        }
    }
    if let Some(h) = height {
        style_parts.push(format!("max-height: {h}px; height: {h}px"));
    }
    if let Some(w) = width {
        style_parts.push(format!("max-width: {w}px"));
    }

    if !style_parts.is_empty() {
        attributes.push(format!(" style=\"{}\"", escape_attribute(&style_parts.join("; "))));
    }

    let suffix = if self_closing { "/>" } else { ">" };
    format!("<img{}{suffix}", attributes.concat())
}

fn build_placeholder_tag(raw_tag: &str) -> String {
    let (width, height) = get_tag_dimensions(raw_tag);
    let (parsed_attributes, self_closing) =
        parse_attributes(raw_tag).unwrap_or_else(|| (Vec::new(), false));
    let mut attributes = Vec::new();
    attributes.push(format!(" src=\"{PLACEHOLDER_IMAGE_DATA_URI}\""));
    attributes.push(" data-meridian-placeholder=\"true\"".to_string());

    let mut has_title = false;
    let mut alt_text = None;
    let mut existing_style = None;

    for attribute in parsed_attributes {
        let name_lower = attribute.name.to_ascii_lowercase();
        if matches!(
            name_lower.as_str(),
            "src" | "srcset" | "imagesrcset" | "xlink:href" | "data-meridian-placeholder"
        ) {
            continue;
        }
        if name_lower == "title" {
            has_title = true;
        }
        if name_lower == "alt" {
            if let Some(ref val) = attribute.value {
                alt_text = Some(val.clone());
            }
        }
        if name_lower == "style" {
            existing_style = attribute.value;
            continue;
        }
        if let Some(val) = attribute.value {
            attributes.push(format!(" {}=\"{}\"", attribute.name, escape_attribute(&val)));
        } else {
            attributes.push(format!(" {}", attribute.name));
        }
    }

    if !has_title {
        if let Some(alt) = alt_text {
            if !alt.trim().is_empty() {
                attributes.push(format!(" title=\"{}\"", escape_attribute(&alt)));
            }
        }
    }

    // Build style preserving aspect ratio and preventing square distortion
    let mut style_parts = Vec::new();
    if let Some(ref style) = existing_style {
        let trimmed = style.trim().trim_end_matches(';').trim();
        if !trimmed.is_empty() {
            style_parts.push(trimmed.to_string());
        }
    }

    match (width, height) {
        (Some(w), Some(h)) => {
            if w > 0 && h > 0 {
                style_parts.push(format!("aspect-ratio: {w} / {h}; max-height: {h}px"));
            }
        }
        (Some(w), None) => {
            if w >= 300 {
                // Wide banner/header with no height attribute (e.g. corp.eml 689px header):
                // Give it a natural wide banner aspect ratio instead of a 1:1 square
                style_parts.push("aspect-ratio: 16 / 5; max-height: 180px".to_string());
            } else if w > 0 {
                style_parts.push(format!("max-height: {w}px"));
            }
        }
        (None, Some(h)) => {
            if h > 0 {
                style_parts.push(format!("max-height: {h}px"));
            }
        }
        (None, None) => {}
    }

    if !style_parts.is_empty() {
        attributes.push(format!(" style=\"{}\"", escape_attribute(&style_parts.join("; "))));
    }

    let suffix = if self_closing { "/>" } else { ">" };
    format!("<img{}{suffix}", attributes.concat())
}

/// Remove externally hosted image elements from already sanitized mail HTML.
///
/// Convenience wrapper calling `block_remote_images_with_options(input, false)`
/// for backward compatibility.
pub fn block_remote_images(input: &str) -> String {
    block_remote_images_with_options(input, false)
}

/// Remove externally hosted image elements or replace them with layout-preserving
/// placeholders from already sanitized mail HTML.
///
/// When `use_placeholders` is true, remote `<img>` tags are converted to lightweight
/// placeholders that preserve original width/height/alt while preventing any remote
/// network requests. Tracking pixels (<= 3px) and remote `<source>` candidates in
/// `<picture>` are stripped, while spacer/layout images are converted to transparent pixels.
pub fn block_remote_images_with_options(input: &str, use_placeholders: bool) -> String {
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
        let name_info = tag_name(raw_tag);
        let is_remote_image = name_info
            .as_ref()
            .is_some_and(|(closing, tag)| !closing && (tag == "img" || tag == "source"))
            && (image_attribute(raw_tag, "src").is_some_and(|source| is_remote_image_uri(&source))
                || image_attribute(raw_tag, "srcset").is_some_and(|source| is_remote_srcset(&source))
                || image_attribute(raw_tag, "xlink:href").is_some_and(|source| is_remote_image_uri(&source)));

        if is_remote_image {
            let is_img = name_info.as_ref().is_some_and(|(_, tag)| tag == "img");
            if use_placeholders && is_img {
                if is_tracking_pixel(raw_tag) {
                    // Tracking pixels are completely stripped
                } else if is_spacer_image(raw_tag) {
                    // Layout and spacer images are replaced with an invisible transparent pixel
                    // without any placeholder frame, preserving the email's table geometry!
                    output.push_str(&build_transparent_spacer_tag(raw_tag));
                } else {
                    // True content images get the GNOME native placeholder frame
                    output.push_str(&build_placeholder_tag(raw_tag));
                }
            }
        } else {
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

/// Extracts a clean, human-readable preview snippet from plain text or raw snippet content,
/// stripping technical noise like image placeholders (`[image: Google]`), markdown/bracketed
/// links (`[link](url)` -> `link`), raw URLs, preheader padding, and email header repetitions.
pub fn clean_mail_snippet(raw: &str) -> String {
    if raw.trim().is_empty() {
        return String::new();
    }

    // 1. Strip zero-width characters and normalize NBSP
    let cleaned_chars: String = raw
        .chars()
        .filter_map(|c| match c {
            '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' | '\u{2060}' | '\u{00AD}' => None,
            '\u{00A0}' => Some(' '),
            other => Some(other),
        })
        .collect();

    // 2. Strip image placeholders like `[image: Google]`, `[IMAGE: ...]`, `[image]`, `[bild: ...]`, `[logo]`, `[cid:...]`
    let without_images = strip_image_placeholders(&cleaned_chars);

    // 3. Transform markdown links: `[anchor text](https://url)` -> `anchor text`
    let without_markdown_links = transform_markdown_links(&without_images);

    // 4. Strip bracketed URLs: `<https://...>`, `[https://...]`, `(https://...)`, `<http://...>`
    let without_bracketed_urls = strip_bracketed_urls(&without_markdown_links);

    // 5. Strip remaining standalone URLs (http://..., https://..., www....)
    let without_urls = strip_standalone_urls(&without_bracketed_urls);

    // 6. Strip email header repetitions at the beginning (e.g. "From: ...", "Subject: ...", "Von: ...")
    let without_headers = strip_leading_headers(&without_urls);

    // 7. Strip markdown bold / italic formatting markers (**bold** -> bold, *italic* -> italic)
    let without_decorations = strip_markdown_decorations(&without_headers);

    // 8. Collapse whitespace into single spaces and normalize punctuation spacing
    let mut words = Vec::new();
    for word in without_decorations.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| c == '-' || c == '_' || c == '|' || c == '~');
        if !trimmed.is_empty() {
            words.push(trimmed);
        }
    }
    let mut collapsed = words.join(" ");
    for punct in [".", ",", "!", "?", ";", ":"] {
        let pattern = format!(" {punct}");
        collapsed = collapsed.replace(&pattern, punct);
    }

    // 9. Truncate to clean word boundary around 130 characters with ellipsis
    truncate_to_word_boundary(&collapsed, 130)
}

/// Convert HTML mail markup into a clean, human-readable preview snippet without technical artifacts.
pub fn clean_mail_snippet_from_html(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let body_html = if let Some(head_start) = lower.find("<head") {
        if let Some(head_close) = lower[head_start..].find("</head>") {
            let after = head_start + head_close + "</head>".len();
            format!("{}{}", &html[..head_start], &html[after..])
        } else {
            html.to_string()
        }
    } else {
        html.to_string()
    };
    let plain = html_to_plain_text(&body_html);
    clean_mail_snippet(&plain)
}

fn strip_image_placeholders(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut i = 0;

    while i < chars.len() {
        let (byte_pos, ch) = chars[i];
        if ch == '[' || ch == '(' {
            let close_ch = if ch == '[' { ']' } else { ')' };
            if let Some(rel_close) = input[byte_pos + 1..].find(close_ch) {
                let close_pos = byte_pos + 1 + rel_close;
                let inner = input[byte_pos + 1..close_pos].trim();
                let inner_lower = inner.to_ascii_lowercase();
                let is_img = [
                    "image", "img", "bild", "photo", "foto", "grafik", "graphic", "icon", "logo", "cid",
                    "attachment", "anhang",
                ]
                .iter()
                .any(|&prefix| {
                    if inner_lower.starts_with(prefix) {
                        let after = &inner_lower[prefix.len()..];
                        after.is_empty()
                            || after.starts_with(':')
                            || after.starts_with(' ')
                            || after.starts_with('=')
                            || after.starts_with('/')
                    } else {
                        false
                    }
                });

                if is_img {
                    let mut next_byte = close_pos + 1;
                    let rest = &input[next_byte..];
                    let trimmed = rest.trim_start();
                    if trimmed.starts_with('(') {
                        if let Some(p_close) = trimmed.find(')') {
                            let p_inner = trimmed[1..p_close].trim().to_ascii_lowercase();
                            if p_inner.starts_with("http://")
                                || p_inner.starts_with("https://")
                                || p_inner.starts_with("www.")
                                || p_inner.starts_with("mailto:")
                            {
                                let ws_len = rest.len() - trimmed.len();
                                next_byte += ws_len + p_close + 1;
                            }
                        }
                    }
                    while i < chars.len() && chars[i].0 < next_byte {
                        i += 1;
                    }
                    continue;
                }
            }
        }
        result.push(ch);
        i += 1;
    }
    result
}

fn transform_markdown_links(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let mut cursor = 0;

    while let Some(rel_open) = input[cursor..].find('[') {
        let open_bracket = cursor + rel_open;
        result.push_str(&input[cursor..open_bracket]);

        if let Some(rel_close) = input[open_bracket + 1..].find(']') {
            let close_bracket = open_bracket + 1 + rel_close;
            let anchor_text = &input[open_bracket + 1..close_bracket];

            let after = &input[close_bracket + 1..];
            let trimmed_after = after.trim_start();
            if trimmed_after.starts_with('(') {
                if let Some(rel_paren_close) = trimmed_after.find(')') {
                    let target_url = trimmed_after[1..rel_paren_close].trim();
                    let lower = target_url.to_ascii_lowercase();
                    let is_url = lower.starts_with("http://")
                        || lower.starts_with("https://")
                        || lower.starts_with("mailto:")
                        || lower.starts_with("tel:")
                        || lower.starts_with("www.");
                    if is_url {
                        result.push_str(anchor_text);
                        let ws_len = after.len() - trimmed_after.len();
                        cursor = close_bracket + 1 + ws_len + rel_paren_close + 1;
                        continue;
                    }
                }
            } else if trimmed_after.starts_with('[') {
                if let Some(rel_ref_close) = trimmed_after.find(']') {
                    let ref_id = trimmed_after[1..rel_ref_close].trim();
                    if ref_id.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '-') {
                        result.push_str(anchor_text);
                        let ws_len = after.len() - trimmed_after.len();
                        cursor = close_bracket + 1 + ws_len + rel_ref_close + 1;
                        continue;
                    }
                }
            }
        }
        result.push('[');
        cursor = open_bracket + 1;
    }
    result.push_str(&input[cursor..]);
    result
}

fn strip_bracketed_urls(input: &str) -> String {
    let mut result = String::with_capacity(input.len());
    let chars: Vec<(usize, char)> = input.char_indices().collect();
    let mut i = 0;

    while i < chars.len() {
        let (byte_pos, ch) = chars[i];
        if ch == '<' || ch == '[' || ch == '(' {
            let close_ch = match ch {
                '<' => '>',
                '[' => ']',
                '(' => ')',
                _ => unreachable!(),
            };
            if let Some(rel_close) = input[byte_pos + 1..].find(close_ch) {
                let close_pos = byte_pos + 1 + rel_close;
                let candidate = input[byte_pos + 1..close_pos].trim();
                let lower = candidate.to_ascii_lowercase();
                let is_url = lower.starts_with("http://")
                    || lower.starts_with("https://")
                    || lower.starts_with("www.")
                    || lower.starts_with("mailto:")
                    || (lower.contains("://") && (lower.contains("http://") || lower.contains("https://")))
                    || ((lower.starts_with("link:")
                        || lower.starts_with("url:")
                        || lower.starts_with("quelle:")
                        || lower.starts_with("siehe:")
                        || lower.starts_with("source:"))
                        && (lower.contains("http://") || lower.contains("https://") || lower.contains("www.")));

                let is_footnote = ch == '[' && !candidate.is_empty() && candidate.chars().all(|c| c.is_ascii_digit());

                if is_url || is_footnote {
                    let next_byte = close_pos + 1;
                    while i < chars.len() && chars[i].0 < next_byte {
                        i += 1;
                    }
                    continue;
                }
            }
        }
        result.push(ch);
        i += 1;
    }
    result
}

fn strip_standalone_urls(input: &str) -> String {
    let mut result = Vec::new();
    for word in input.split_whitespace() {
        let trimmed = word.trim_matches(|c: char| {
            c == '<' || c == '>' || c == '(' || c == ')' || c == '[' || c == ']' || c == '{' || c == '}' || c == '"' || c == '\''
        });
        let candidate = trimmed.trim_end_matches(|c: char| c == '.' || c == ',' || c == ';' || c == ':');
        let lower = candidate.to_ascii_lowercase();
        let is_url = lower.starts_with("http://")
            || lower.starts_with("https://")
            || lower.starts_with("www.")
            || (lower.starts_with("mailto:") && lower.contains('@'));
        if !is_url {
            result.push(word);
        }
    }
    result.join(" ")
}

fn strip_leading_headers(input: &str) -> String {
    let mut lines = input.lines().peekable();
    let mut skipped = false;

    while let Some(&line) = lines.peek() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            lines.next();
            continue;
        }

        let is_sep = (trimmed.starts_with("---") && trimmed.ends_with("---"))
            || (trimmed.starts_with("===") && trimmed.ends_with("==="))
            || (trimmed.starts_with("___") && trimmed.ends_with("___"));
        if is_sep {
            lines.next();
            skipped = true;
            continue;
        }

        let lower = trimmed.to_ascii_lowercase();
        let is_hdr = lower.starts_with("von:")
            || lower.starts_with("from:")
            || lower.starts_with("gesendet:")
            || lower.starts_with("sent:")
            || lower.starts_with("datum:")
            || lower.starts_with("date:")
            || lower.starts_with("an:")
            || lower.starts_with("to:")
            || lower.starts_with("cc:")
            || lower.starts_with("betreff:")
            || lower.starts_with("subject:")
            || lower.starts_with("ursprüngliche nachricht")
            || lower.starts_with("original message");

        if is_hdr {
            lines.next();
            skipped = true;
            continue;
        }

        break;
    }

    if skipped {
        let remaining: Vec<&str> = lines.collect();
        if !remaining.is_empty() {
            let joined = remaining.join("\n");
            if !joined.trim().is_empty() {
                return joined;
            }
        }
    }
    input.to_string()
}

fn strip_markdown_decorations(input: &str) -> String {
    let s = input.replace("**", "").replace("__", "");
    let mut result = Vec::new();
    for line in s.lines() {
        let trimmed = line.trim_start();
        let stripped = trimmed
            .trim_start_matches('#')
            .trim_start_matches(|c: char| c == '*' || c == '-' || c == '•');
        result.push(stripped.trim());
    }
    result.join(" ")
}

fn truncate_to_word_boundary(text: &str, max_chars: usize) -> String {
    let count = text.chars().count();
    if count <= max_chars {
        return text.to_string();
    }
    let prefix: String = text.chars().take(max_chars).collect();
    let min_boundary = (max_chars * 3) / 5;
    if let Some(last_space) = prefix.rfind(' ') {
        if prefix[..last_space].chars().count() >= min_boundary {
            let trimmed = prefix[..last_space].trim_end_matches(|c: char| {
                c == ',' || c == '.' || c == ';' || c == ':' || c == '-' || c == '!' || c == '?'
            });
            return format!("{trimmed}…");
        }
    }
    format!("{prefix}…")
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

fn sanitize_tag(
    raw_tag: &str,
    closing: bool,
    tag_name: &str,
    allow_remote_resources: bool,
) -> Option<String> {
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
            if let Some(value) = value
                .as_deref()
                .and_then(|v| sanitize_css(v, allow_remote_resources))
            {
                attributes.push(format!(" style=\"{}\"", escape_attribute(&value)));
            }
            continue;
        }
        if is_blocked_attribute(&attribute_name, allow_remote_resources) {
            continue;
        }
        if matches!(attribute_name.as_str(), "srcset" | "imagesrcset") {
            if let Some(value) = value.as_deref() {
                if let Some(safe_srcset) = sanitize_srcset(value, tag_name) {
                    attributes.push(format!(
                        " {attribute_name}=\"{}\"",
                        escape_attribute(&safe_srcset)
                    ));
                }
            }
            continue;
        }
        if attribute_name == "background" {
            if let Some(value) = value.as_deref() {
                if matches!(tag_name, "td" | "th" | "table" | "body")
                    && is_safe_uri(value, tag_name, &attribute_name)
                {
                    let normalized = normalize_protocol_relative_uri(value);
                    attributes.push(format!(" background=\"{}\"", escape_attribute(&normalized)));
                }
            }
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
            if is_uri_attribute(&attribute_name) {
                if !is_safe_uri(value, tag_name, &attribute_name) {
                    continue;
                }
                let normalized = normalize_protocol_relative_uri(value);
                attributes.push(format!(
                    " {attribute_name}=\"{}\"",
                    escape_attribute(&normalized)
                ));
                continue;
            }
            attributes.push(format!(" {attribute_name}=\"{}\"", escape_attribute(value)));
        } else {
            attributes.push(format!(" {attribute_name}"));
        }
    }

    let tag_suffix = if self_closing
        || matches!(
            tag_name,
            "br" | "hr" | "img" | "col" | "source" | "wbr" | "track"
        ) {
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

fn sanitize_css(value: &str, allow_remote_resources: bool) -> Option<String> {
    let trimmed = strip_css_comments(value);
    let trimmed = trimmed.trim();
    if trimmed.is_empty() {
        return None;
    }

    if trimmed.starts_with('@') || has_top_level_css_brace(trimmed) {
        sanitize_stylesheet(trimmed, allow_remote_resources)
    } else {
        sanitize_inline_css(trimmed, allow_remote_resources)
    }
}

fn sanitize_stylesheet(value: &str, allow_remote_resources: bool) -> Option<String> {
    let without_comments = strip_css_comments(value);
    let sanitized = sanitize_css_rules(&without_comments, allow_remote_resources);
    (!sanitized.trim().is_empty()).then_some(sanitized)
}

fn sanitize_inline_css(value: &str, allow_remote_resources: bool) -> Option<String> {
    let without_comments = strip_css_comments(value);
    let declarations = sanitize_css_declarations(&without_comments, allow_remote_resources)?;
    let declarations = declarations.trim().trim_end_matches(';').trim();
    (!declarations.is_empty()).then_some(declarations.to_string())
}

fn sanitize_css_rules(value: &str, allow_remote_resources: bool) -> String {
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
                    sanitize_css_block(prelude, &value[delimiter_position + 1..close], allow_remote_resources)
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

fn sanitize_css_block(prelude: &str, body: &str, allow_remote_resources: bool) -> Option<String> {
    if css_contains_blocked_construct(prelude, allow_remote_resources) {
        return None;
    }

    if prelude.starts_with('@') {
        let at_rule = css_at_rule_name(prelude)?;
        if !ALLOWED_CSS_AT_RULES.contains(&at_rule.as_str()) {
            return None;
        }

        let sanitized_body = if at_rule == "page" {
            sanitize_css_declarations(body, allow_remote_resources)?
        } else {
            let rules = sanitize_css_rules(body, allow_remote_resources);
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

    let declarations = sanitize_css_declarations(body, allow_remote_resources)?;
    Some(format!("{} {{ {} }}", prelude.trim(), declarations.trim()))
}

fn sanitize_css_declarations(value: &str, allow_remote_resources: bool) -> Option<String> {
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
            || css_contains_blocked_construct(property_value, allow_remote_resources)
        {
            continue;
        }
        let property_value = if allow_remote_resources {
            normalize_css_urls(property_value)
        } else {
            property_value.to_string()
        };
        output.push_str(property);
        output.push_str(": ");
        output.push_str(&property_value);
        output.push_str("; ");
    }

    let declaration = value[start..].trim();
    if let Some((property, property_value)) = split_css_declaration(declaration) {
        let property = property.trim();
        let property_value = property_value.trim();
        if is_safe_css_property(property)
            && !property_value.is_empty()
            && !css_contains_blocked_construct(property_value, allow_remote_resources)
        {
            let property_value = if allow_remote_resources {
                normalize_css_urls(property_value)
            } else {
                property_value.to_string()
            };
            output.push_str(property);
            output.push_str(": ");
            output.push_str(&property_value);
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

fn css_contains_blocked_construct(value: &str, allow_remote_resources: bool) -> bool {
    let normalized = normalize_css_for_policy(value);
    let has_blocked_token = [
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
    .any(|token| normalized.contains(token));
    if has_blocked_token {
        return true;
    }

    let blocked_functions: &[&str] = if allow_remote_resources {
        &[
            "expression",
            "image-set",
            "-webkit-image-set",
            "cross-fade",
            "paint",
        ]
    } else {
        &[
            "url",
            "expression",
            "image-set",
            "-webkit-image-set",
            "cross-fade",
            "paint",
        ]
    };

    if blocked_functions
        .iter()
        .any(|function| css_function_present(&normalized, function))
    {
        return true;
    }

    if allow_remote_resources && css_function_present(&normalized, "url") {
        if !css_urls_are_safe(value) {
            return true;
        }
    }

    false
}

fn is_safe_css_url(url: &str) -> bool {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return true;
    }
    let lower = normalize_uri_prefix(trimmed);
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
        return lower.starts_with("data:image/");
    }
    if lower.starts_with("//")
        || lower.starts_with("http://")
        || lower.starts_with("https://")
        || lower.starts_with("cid:")
    {
        return true;
    }
    !lower.contains(':')
}

fn css_urls_are_safe(value: &str) -> bool {
    let bytes = value.as_bytes();
    let lower = value.to_ascii_lowercase();
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(rel_pos) = lower[cursor..].find("url(") else {
            break;
        };
        let start = cursor + rel_pos + 4;
        let mut close_paren = None;
        let mut inner_cursor = start;
        let mut quote = None;
        while inner_cursor < bytes.len() {
            let b = bytes[inner_cursor];
            if let Some(q) = quote {
                if b == b'\\' {
                    inner_cursor += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            } else if b == b'\'' || b == b'"' {
                quote = Some(b);
            } else if b == b')' {
                close_paren = Some(inner_cursor);
                break;
            }
            inner_cursor += 1;
        }
        let Some(end) = close_paren else {
            return false;
        };
        let inner = value[start..end].trim();
        let unquoted = inner.trim_matches(|c| c == '\'' || c == '"').trim();
        if !is_safe_css_url(unquoted) {
            return false;
        }
        cursor = end + 1;
    }
    true
}

fn normalize_css_urls(value: &str) -> String {
    let bytes = value.as_bytes();
    let lower = value.to_ascii_lowercase();
    let mut output = String::with_capacity(value.len());
    let mut cursor = 0;
    while cursor < bytes.len() {
        let Some(rel_pos) = lower[cursor..].find("url(") else {
            output.push_str(&value[cursor..]);
            break;
        };
        let start = cursor + rel_pos;
        output.push_str(&value[cursor..start + 4]);
        let inner_start = start + 4;
        let mut close_paren = None;
        let mut inner_cursor = inner_start;
        let mut quote = None;
        while inner_cursor < bytes.len() {
            let b = bytes[inner_cursor];
            if let Some(q) = quote {
                if b == b'\\' {
                    inner_cursor += 2;
                    continue;
                }
                if b == q {
                    quote = None;
                }
            } else if b == b'\'' || b == b'"' {
                quote = Some(b);
            } else if b == b')' {
                close_paren = Some(inner_cursor);
                break;
            }
            inner_cursor += 1;
        }
        if let Some(end) = close_paren {
            let inner = value[inner_start..end].trim();
            let (quote_char, raw_url) = if (inner.starts_with('\'')
                && inner.ends_with('\'')
                && inner.len() >= 2)
                || (inner.starts_with('"') && inner.ends_with('"') && inner.len() >= 2)
            {
                (Some(&inner[..1]), inner[1..inner.len() - 1].trim())
            } else {
                (None, inner)
            };
            let normalized_url = normalize_protocol_relative_uri(raw_url);
            if let Some(q) = quote_char {
                output.push_str(q);
                output.push_str(&normalized_url);
                output.push_str(q);
            } else {
                output.push_str(&normalized_url);
            }
            output.push(')');
            cursor = end + 1;
        } else {
            output.push_str(&value[start + 4..]);
            break;
        }
    }
    output
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
    image_attribute(raw_tag, "src")
}

pub(crate) fn image_attribute(raw_tag: &str, name: &str) -> Option<String> {
    parse_attributes(raw_tag)?
        .0
        .into_iter()
        .find(|attribute| attribute.name == name)
        .and_then(|attribute| attribute.value)
}

pub fn is_remote_srcset(value: &str) -> bool {
    value.split(',').any(|entry| {
        let entry = entry.trim();
        let url = entry.split_whitespace().next().unwrap_or(entry);
        is_remote_image_uri(url)
    })
}

pub fn normalize_protocol_relative_uri(uri: &str) -> String {
    let trimmed = uri.trim();
    if trimmed.starts_with("//") {
        format!("https:{trimmed}")
    } else {
        trimmed.to_string()
    }
}

fn sanitize_srcset(value: &str, tag_name: &str) -> Option<String> {
    if tag_name != "img" && tag_name != "source" {
        return None;
    }
    let mut parts = Vec::new();
    for candidate in value.split(',') {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            continue;
        }
        let mut tokens = trimmed.split_whitespace();
        let Some(url) = tokens.next() else { continue };
        if !is_safe_uri(url, tag_name, "srcset") {
            return None;
        }
        let normalized_url = normalize_protocol_relative_uri(url);
        let descriptor = tokens.collect::<Vec<_>>().join(" ");
        if descriptor.is_empty() {
            parts.push(normalized_url);
        } else {
            parts.push(format!("{normalized_url} {descriptor}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join(", "))
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

fn is_blocked_attribute(attribute_name: &str, allow_remote_resources: bool) -> bool {
    if attribute_name.starts_with("on") {
        return true;
    }
    if allow_remote_resources && matches!(attribute_name, "background" | "srcset" | "imagesrcset") {
        return false;
    }
    matches!(
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
        "src" | "xlink:href" | "srcset" | "imagesrcset" => {
            tag_name == "img" || tag_name == "source"
        }
        "background" => matches!(tag_name, "td" | "th" | "table" | "body"),
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
        "cid" => {
            (tag_name == "img" || tag_name == "source")
                && matches!(attribute_name, "src" | "xlink:href" | "srcset")
        }
        _ => false,
    }
}

fn is_allowed_data_image(value: &str, tag_name: &str, attribute_name: &str) -> bool {
    if !(tag_name == "img" || tag_name == "source")
        || !matches!(attribute_name, "src" | "xlink:href" | "srcset")
    {
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

    #[test]
    fn preserves_picture_and_source_tags_without_truncating_on_void_elements() {
        let html = r#"<picture><source srcset="https://cdn.example/image.webp"><img src="https://cdn.example/image.png"></picture><p>Remaining text</p>"#;
        let sanitized = sanitize_html(html);
        assert!(sanitized.contains("<picture>"));
        assert!(sanitized.contains("<source"));
        assert!(sanitized.contains("<img"));
        assert!(sanitized.contains("<p>Remaining text</p>"));
    }

    #[test]
    fn strips_xml_and_noscript_blocks_without_leaking_internal_text() {
        let html = r#"<head><xml><o:OfficeDocumentSettings>Settings 1</o:OfficeDocumentSettings></xml><noscript>Enable JS</noscript></head><body><p>Visible Content</p></body>"#;
        let sanitized = sanitize_html(html);
        assert!(!sanitized.contains("Settings 1"));
        assert!(!sanitized.contains("Enable JS"));
        assert!(sanitized.contains("<p>Visible Content</p>"));
    }

    #[test]
    fn clean_mail_snippet_strips_image_placeholders_and_bracketed_urls() {
        let text = "[image: Google] Dein Sicherheitscode lautet 482910. Bitte nicht weitergeben.";
        assert_eq!(
            clean_mail_snippet(text),
            "Dein Sicherheitscode lautet 482910. Bitte nicht weitergeben."
        );

        let img_case = "[IMAGE: Company Logo] [image] Willkommen im Team!";
        assert_eq!(clean_mail_snippet(img_case), "Willkommen im Team!");

        let german_img = "[Bild: Foto von Max] Guten Tag Herr Mustermann";
        assert_eq!(
            clean_mail_snippet(german_img),
            "Guten Tag Herr Mustermann"
        );
    }

    #[test]
    fn clean_mail_snippet_transforms_markdown_links_and_strips_urls() {
        let md = "Besuchen Sie [unsere Hilfeseite](https://support.example.com/help?id=123) für mehr Infos.";
        assert_eq!(
            clean_mail_snippet(md),
            "Besuchen Sie unsere Hilfeseite für mehr Infos."
        );

        let bracketed = "Hier klicken <https://example.com/very/long/url> oder (https://alt.example.com)";
        assert_eq!(clean_mail_snippet(bracketed), "Hier klicken oder");

        let standalone = "Projektstatus ist grün. Siehe https://jira.example.org/browse/DEV-42 für Details.";
        assert_eq!(
            clean_mail_snippet(standalone),
            "Projektstatus ist grün. Siehe für Details."
        );
    }

    #[test]
    fn clean_mail_snippet_strips_preheader_padding_and_zero_width_chars() {
        let preheader_padding = "\u{200c}\u{200b}\u{a0}\u{a0}\u{200c}\u{a0}   Ihr monatlicher Kontoauszug ist verfügbar.   ";
        assert_eq!(
            clean_mail_snippet(preheader_padding),
            "Ihr monatlicher Kontoauszug ist verfügbar."
        );
    }

    #[test]
    fn clean_mail_snippet_from_html_cleans_html_entities_and_markup() {
        let html = "<html><body><p>[image: Newsletter] <strong>Wichtige Ankündigung:</strong> Neue Features online! &amp; mehr.</p></body></html>";
        assert_eq!(
            clean_mail_snippet_from_html(html),
            "Wichtige Ankündigung: Neue Features online! & mehr."
        );
    }

    #[test]
    fn clean_mail_snippet_truncates_at_word_boundary() {
        let long_text = "Dies ist ein sehr langer Text mit vielen Wörtern der definitiv die maximale Längenbegrenzung von hundertdreißig Zeichen überschreiten wird damit das Verhalten bei der Wortkürzung überprüft werden kann.";
        let snippet = clean_mail_snippet(long_text);
        assert!(snippet.ends_with('…'));
        assert!(snippet.chars().count() <= 135);
        // Ensure no word was cut in half right before ellipsis
        let before_ellipsis = snippet.trim_end_matches('…');
        assert!(!before_ellipsis.ends_with(' '));
    }

    #[test]
    fn clean_mail_snippet_handles_complex_noise_and_links() {
        // Image placeholder with attached link
        let img_link = "[image: Google](https://google.com) Willkommen bei Ihrem neuen Konto.";
        assert_eq!(clean_mail_snippet(img_link), "Willkommen bei Ihrem neuen Konto.");

        let img_link_spaced = "[image: Logo] (https://cdn.example.com/logo.png) Unser Quartalsbericht";
        assert_eq!(clean_mail_snippet(img_link_spaced), "Unser Quartalsbericht");

        // Parenthesized image
        let paren_img = "(image: Google) Hallo Welt";
        assert_eq!(clean_mail_snippet(paren_img), "Hallo Welt");

        // Markdown link with space before parenthesis
        let spaced_md = "Besuchen Sie [unsere Webseite] (https://example.com) für Details.";
        assert_eq!(clean_mail_snippet(spaced_md), "Besuchen Sie unsere Webseite für Details.");

        // Non-URL bracket followed by URL bracket (the bug where first non-URL bracket stopped scanning)
        let mixed_brackets = "Hallo (Hinweis) hier ist der Link (https://example.com/page). Schönen Tag!";
        assert_eq!(
            clean_mail_snippet(mixed_brackets),
            "Hallo (Hinweis) hier ist der Link. Schönen Tag!"
        );

        // Footnote references in plain text
        let footnote = "Wichtige Neuigkeiten [1] zum Projektstatus [2]. Weitere Infos folgen.";
        assert_eq!(
            clean_mail_snippet(footnote),
            "Wichtige Neuigkeiten zum Projektstatus. Weitere Infos folgen."
        );
    }

    #[test]
    fn test_clean_mail_snippet_from_html_strips_head_and_artifacts() {
        let html = r#"<html><head><title>Re: Status Update</title><style>.msg { color: blue; }</style></head><body><div class="preheader" style="display:none">[image: Google]</div><p>Hallo Martin, hier ist die Zusammenfassung unserer Besprechung.</p></body></html>"#;
        assert_eq!(
            clean_mail_snippet_from_html(html),
            "Hallo Martin, hier ist die Zusammenfassung unserer Besprechung."
        );
    }

    #[test]
    fn test_sanitize_html_with_options_remote_resources() {
        let html = r#"<img src="//images.example.com/pic.jpg" srcset="//images.example.com/pic-1x.jpg 1x, https://images.example.com/pic-2x.jpg 2x"><table background="//images.example.com/bg.png"><tr><td style="background-image: url('//images.example.com/table-bg.png')">Content</td></tr></table><style>.hero { background: url(//images.example.com/hero.jpg); }</style>"#;

        // When remote resources are disabled:
        let blocked = sanitize_html_with_options(html, false);
        assert!(!blocked.contains("srcset="));
        assert!(!blocked.contains("background="));
        assert!(!blocked.contains("url("));

        // When remote resources are enabled:
        let allowed = sanitize_html_with_options(html, true);
        assert!(allowed.contains("src=\"https://images.example.com/pic.jpg\""));
        assert!(allowed.contains("srcset=\"https://images.example.com/pic-1x.jpg 1x, https://images.example.com/pic-2x.jpg 2x\""));
        assert!(allowed.contains("background=\"https://images.example.com/bg.png\""));
        assert!(allowed.contains("background-image: url('https://images.example.com/table-bg.png')"));
        assert!(allowed.contains("background: url(https://images.example.com/hero.jpg)"));

        // Dangerous CSS urls are still rejected:
        let evil = r#"<div style="background: url('javascript:alert(1)')">Evil</div>"#;
        let evil_sanitized = sanitize_html_with_options(evil, true);
        assert!(!evil_sanitized.contains("javascript"));
        assert!(!evil_sanitized.contains("url("));
    }

    #[test]
    fn test_block_remote_images_with_srcset_and_sources() {
        let html = r#"<picture><source srcset="https://cdn.example/pic.webp"><img src="https://cdn.example/pic.jpg" srcset="https://cdn.example/pic.jpg 1x"></picture><img src="data:image/png;base64,AAAA">"#;
        let sanitized = sanitize_html_with_options(html, true);
        let blocked = block_remote_images(&sanitized);
        assert!(!blocked.contains("cdn.example"));
        assert!(blocked.contains("data:image/png;base64,AAAA"));
    }

    #[test]
    fn test_strips_title_tag_and_contents() {
        let html = r#"<html><head><title>Join us in our next event: GDG Rhein-Main Networking & Drinks September 2026 </title><style>a { color: blue; }</style></head><body><p>Visible content</p></body></html>"#;
        let sanitized = sanitize_html(html);
        assert!(!sanitized.contains("<title>"));
        assert!(!sanitized.contains("</title>"));
        assert!(!sanitized.contains("Join us in our next event"));
        assert!(sanitized.contains("Visible content"));
        assert!(sanitized.contains("<style>"));
    }

    #[test]
    fn test_join_us_eml_does_not_leak_title() {
        let eml_content = r#"From: GDG <events@example.com>
Subject: Community Event
Content-Type: text/html; charset=utf-8

<!DOCTYPE html>
<html>
<head>
<title>Join us in our next event: GDG Rhein-Main Networking & Drinks September 2026 </title>
<style>body { margin: 0; }</style>
</head>
<body>
<p>Welcome to the community event!</p>
</body>
</html>"#;
        let html_start = eml_content.find("<html").expect("HTML start tag");
        let html_end = eml_content.find("</html>").expect("HTML end tag") + 7;
        let html = &eml_content[html_start..html_end];
        let sanitized = sanitize_html(html);
        assert!(
            !sanitized.contains("Join us in our next event"),
            "Sanitized HTML must not contain the <title> text from Join us.eml"
        );
        assert!(!sanitized.contains("<title>"));
        assert!(!sanitized.contains("</title>"));
    }

    #[test]
    fn test_block_remote_images_with_placeholders_preserves_dimensions_and_alt() {
        let html = r#"<img src="https://cdn.example/banner.jpg" width="600" height="200" alt="Sommer-Aktion" class="hero">"#;
        let blocked = block_remote_images_with_options(html, true);
        assert!(!blocked.contains("cdn.example"));
        assert!(blocked.contains("data-meridian-placeholder=\"true\""));
        assert!(blocked.contains("width=\"600\""));
        assert!(blocked.contains("height=\"200\""));
        assert!(blocked.contains("alt=\"Sommer-Aktion\""));
        assert!(blocked.contains("title=\"Sommer-Aktion\""));
        assert!(blocked.contains("class=\"hero\""));
        assert!(blocked.contains("src=\"data:image/svg+xml;base64,"));
    }

    #[test]
    fn test_block_remote_images_with_placeholders_drops_tracking_pixels() {
        let html = r#"<img src="https://tracker.example/pixel.gif" width="1" height="1"><img src="https://tracker.example/hidden" style="display:none"><img src="https://tracker.example/zero" width="0" height="0">"#;
        let blocked = block_remote_images_with_options(html, true);
        assert_eq!(blocked, "");
    }

    #[test]
    fn test_block_remote_images_with_placeholders_drops_remote_sources() {
        let html = r#"<picture><source srcset="https://cdn.example/pic.webp"><img src="https://cdn.example/pic.jpg" width="100"></picture>"#;
        let blocked = block_remote_images_with_options(html, true);
        assert!(!blocked.contains("<source"));
        assert!(blocked.contains("<img"));
        assert!(blocked.contains("data-meridian-placeholder=\"true\""));
        assert!(blocked.contains("width=\"100\""));
    }

    #[test]
    fn test_block_remote_images_without_placeholders_drops_all_tags() {
        let html = r#"<img src="https://cdn.example/pic.jpg" width="100">"#;
        let blocked = block_remote_images_with_options(html, false);
        assert_eq!(blocked, "");
    }

    #[test]
    fn test_placeholder_image_data_uri_is_valid_svg() {
        assert!(PLACEHOLDER_IMAGE_DATA_URI.starts_with("data:image/svg+xml;base64,"));
        let b64 = &PLACEHOLDER_IMAGE_DATA_URI["data:image/svg+xml;base64,".len()..];
        let bytes = glib::Bytes::from_owned(glib::base64_decode(b64));
        let decoded_str = std::str::from_utf8(&bytes).expect("Valid UTF-8 SVG string");
        assert!(decoded_str.starts_with("<svg "), "Must start with <svg tag");
        assert!(decoded_str.ends_with("</svg>"), "Must be closed with </svg> closing tag");
        assert!(decoded_str.contains("<clipPath"), "Must contain clipPath to prevent protrusion");
        assert!(decoded_str.contains("opacity=\"0.55\""), "Must use group opacity to avoid darker overlapping strokes");
    }

    #[test]
    fn test_block_remote_images_handles_spacers_and_layout_images() {
        // 1. Classic spacer.gif with dimensions: must become transparent pixel without placeholder styling
        let spacer_html = r#"<img src="https://cdn.example/images/spacer.gif" width="15" height="25">"#;
        let spacer_blocked = block_remote_images_with_options(spacer_html, true);
        assert!(spacer_blocked.contains(TRANSPARENT_PIXEL_DATA_URI), "Spacer must use transparent GIF");
        assert!(!spacer_blocked.contains("data-meridian-placeholder"), "Spacer must NOT have placeholder attribute");
        assert!(spacer_blocked.contains("width=\"15\""), "Spacer must keep width");
        assert!(spacer_blocked.contains("height=\"25\""), "Spacer must keep height");

        // 2. Vertical margin spacer table column: width <= 20 without alt text
        let margin_html = r#"<img src="https://cdn.example/col_pad.png" width="12" height="80">"#;
        let margin_blocked = block_remote_images_with_options(margin_html, true);
        assert!(margin_blocked.contains(TRANSPARENT_PIXEL_DATA_URI));
        assert!(!margin_blocked.contains("data-meridian-placeholder"));

        // 3. Thin horizontal divider line: height <= 4
        let line_html = r#"<img src="https://cdn.example/rule.png" width="500" height="2">"#;
        let line_blocked = block_remote_images_with_options(line_html, true);
        assert!(line_blocked.contains(TRANSPARENT_PIXEL_DATA_URI));
        assert!(!line_blocked.contains("data-meridian-placeholder"));

        // 4. Tracking pixel with width/height <= 3: must be completely stripped
        let track_html = r#"<img src="https://tracker.example/pixel" width="2" height="2">"#;
        let track_blocked = block_remote_images_with_options(track_html, true);
        assert_eq!(track_blocked, "");

        // 5. True content image: must have data-meridian-placeholder and SVG placeholder with aspect ratio
        let content_html = r#"<img src="https://cdn.example/asus_laptop.jpg" width="350" height="250" alt="ASUS Laptop">"#;
        let content_blocked = block_remote_images_with_options(content_html, true);
        assert!(content_blocked.contains("data-meridian-placeholder=\"true\""));
        assert!(content_blocked.contains(PLACEHOLDER_IMAGE_DATA_URI));
        assert!(content_blocked.contains("alt=\"ASUS Laptop\""));
        assert!(content_blocked.contains("aspect-ratio: 350 / 250"));
        assert!(content_blocked.contains("max-height: 250px"));
    }

    #[test]
    fn test_block_remote_images_corp_eml_patterns() {
        // 1. Separator between sections: 689x12
        let sep_html = r#"<img src="https://cdn.example/separator_b.gif" style="display:block" width="689" height="12">"#;
        let sep_blocked = block_remote_images_with_options(sep_html, true);
        assert!(sep_blocked.contains(TRANSPARENT_PIXEL_DATA_URI));
        assert!(!sep_blocked.contains("data-meridian-placeholder"));
        assert!(sep_blocked.contains("max-height: 12px; height: 12px"));
        assert!(sep_blocked.contains("max-width: 689px"));

        // 2. Wide banner with no height specified (like corp.eml header):
        let hdr_html = r#"<img src="https://cdn.example/header.png" style="display:block" width="689">"#;
        let hdr_blocked = block_remote_images_with_options(hdr_html, true);
        assert!(hdr_blocked.contains("data-meridian-placeholder=\"true\""));
        assert!(hdr_blocked.contains("aspect-ratio: 16 / 5"));
        assert!(hdr_blocked.contains("max-height: 180px"));

        // 3. Rectangular product banner: 322x155
        let banner_html = r#"<img src="https://cdn.example/product.png" style="display:block;border:1px solid #ddd" width="322" height="155">"#;
        let banner_blocked = block_remote_images_with_options(banner_html, true);
        assert!(banner_blocked.contains("data-meridian-placeholder=\"true\""));
        assert!(banner_blocked.contains("aspect-ratio: 322 / 155"));
        assert!(banner_blocked.contains("max-height: 155px"));
        assert!(banner_blocked.contains("border:1px solid #ddd"));
    }
}

