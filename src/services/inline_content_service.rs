// SPDX-License-Identifier: GPL-3.0-or-later
//
// Safe resolution of MIME inline images referenced by `cid:` URLs.
//
// The HTML viewer has no filesystem base URI and must not be given arbitrary
// attachment paths.  Only an explicitly parsed Content-ID can be resolved,
// and only for small raster image types.  The resulting data URI is passed
// through the HTML sanitizer before it reaches WebKit.

use std::collections::HashMap;
use std::sync::Arc;

const MAX_INLINE_IMAGE_BYTES: usize = 10 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InlineImage {
    pub content_id: String,
    pub mime_type: String,
    pub content: Arc<[u8]>,
}

impl InlineImage {
    pub fn new(
        content_id: impl Into<String>,
        mime_type: impl Into<String>,
        content: impl Into<Arc<[u8]>>,
    ) -> Self {
        Self {
            content_id: content_id.into(),
            mime_type: mime_type.into(),
            content: content.into(),
        }
    }
}

/// Replace `cid:` references in HTML attributes with safe in-memory image
/// data.  Unknown IDs and references outside a tag are left untouched so the
/// normal sanitizer can remove or display them according to its own policy.
pub fn resolve_cid_images(input: &str, images: &[InlineImage]) -> String {
    let image_map = images
        .iter()
        .filter_map(|image| {
            let content_id = normalize_content_id(&image.content_id);
            if content_id.is_empty() {
                return None;
            }
            let mime_type = normalized_image_mime_type(&image.mime_type)?;
            if image.content.is_empty() || image.content.len() > MAX_INLINE_IMAGE_BYTES {
                return None;
            }
            Some((content_id, format_data_uri(&mime_type, &image.content)))
        })
        .collect::<HashMap<_, _>>();

    if image_map.is_empty() || input.is_empty() {
        return input.to_string();
    }

    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while cursor < input.len() {
        let Some(start) = find_cid_scheme(input, cursor) else {
            output.push_str(&input[cursor..]);
            break;
        };
        output.push_str(&input[cursor..start]);

        let Some(end) = cid_reference_end(input, start + 4) else {
            output.push_str(&input[start..]);
            break;
        };
        let reference = &input[start + 4..end];
        if is_uri_attribute_context(input, start)
            && let Some(data_uri) = image_map.get(&normalize_content_id(reference))
        {
            output.push_str(data_uri);
        } else {
            output.push_str(&input[start..end]);
        }
        cursor = end;
    }
    output
}

fn find_cid_scheme(input: &str, from: usize) -> Option<usize> {
    input[from..].char_indices().find_map(|(offset, _)| {
        let index = from + offset;
        input
            .get(index..index + 4)
            .filter(|scheme| scheme.eq_ignore_ascii_case("cid:"))
            .map(|_| index)
    })
}

fn cid_reference_end(input: &str, start: usize) -> Option<usize> {
    if start >= input.len() {
        return None;
    }
    if input[start..].starts_with('<') {
        let relative_end = input[start + 1..].find('>')?;
        return Some(start + 1 + relative_end + 1);
    }

    let end = input[start..]
        .char_indices()
        .find(|(_, character)| {
            character.is_ascii_whitespace()
                || matches!(character, '\'' | '"' | '<' | '>' | ')' | ']' | ';')
        })
        .map(|(offset, _)| start + offset)
        .unwrap_or(input.len());
    (end > start).then_some(end)
}

fn is_uri_attribute_context(input: &str, start: usize) -> bool {
    let Some(tag_start) = input[..start].rfind('<') else {
        return false;
    };
    if input[tag_start..start].contains('>') {
        return false;
    }
    let tag_prefix = &input[tag_start..start];
    let Some(equal) = tag_prefix.rfind('=') else {
        return false;
    };
    let attribute_end = tag_prefix[..equal].trim_end();
    let attribute_start = attribute_end
        .rfind(|character: char| character.is_ascii_whitespace() || character == '<')
        .map(|index| index + 1)
        .unwrap_or(0);
    matches!(
        attribute_end[attribute_start..]
            .to_ascii_lowercase()
            .as_str(),
        "src" | "xlink:href"
    )
}

fn normalize_content_id(value: &str) -> String {
    let trimmed = value.trim().trim_start_matches('<').trim_end_matches('>');
    percent_decode(trimmed).to_ascii_lowercase()
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = String::with_capacity(value.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let high = hex_value(bytes[index + 1]);
            let low = hex_value(bytes[index + 2]);
            if let (Some(high), Some(low)) = (high, low) {
                output.push((high << 4 | low) as char);
                index += 3;
                continue;
            }
        }
        output.push(bytes[index] as char);
        index += 1;
    }
    output
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

fn normalized_image_mime_type(value: &str) -> Option<String> {
    let mime_type = value
        .split(';')
        .next()
        .unwrap_or(value)
        .trim()
        .to_ascii_lowercase();
    matches!(
        mime_type.as_str(),
        "image/bmp" | "image/gif" | "image/jpeg" | "image/png" | "image/webp" | "image/x-icon"
    )
    .then_some(mime_type)
}

fn format_data_uri(mime_type: &str, content: &[u8]) -> String {
    format!("data:{mime_type};base64,{}", base64_encode(content))
}

fn base64_encode(content: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity(content.len().div_ceil(3) * 4);
    for chunk in content.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied();
        let third = chunk.get(2).copied();
        output.push(TABLE[(first >> 2) as usize] as char);
        output.push(TABLE[((first & 0x03) << 4 | second.unwrap_or(0) >> 4) as usize] as char);
        output.push(match second {
            Some(second) => {
                TABLE[((second & 0x0f) << 2 | third.unwrap_or(0) >> 6) as usize] as char
            }
            None => '=',
        });
        output.push(match third {
            Some(third) => TABLE[(third & 0x3f) as usize] as char,
            None => '=',
        });
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_known_inline_raster_image_only_in_uri_attributes() {
        let image = InlineImage::new("logo@example.test", "image/png", b"PNG".to_vec());
        let html = r#"<img src="cid:logo%40example.test"><a href="cid:logo@example.test">cid:logo@example.test</a>"#;

        let resolved = resolve_cid_images(html, &[image]);

        assert!(resolved.contains("src=\"data:image/png;base64,UE5H\""));
        assert!(resolved.contains("href=\"cid:logo@example.test\""));
        assert!(resolved.contains(">cid:logo@example.test</a>"));
    }

    #[test]
    fn accepts_angle_bracket_and_case_variants() {
        let image = InlineImage::new(
            "<Logo@Example.Test>",
            "image/jpeg; charset=binary",
            vec![0xff],
        );

        let resolved = resolve_cid_images(r#"<img src='CID:<logo@example.test>'>"#, &[image]);

        assert!(resolved.contains("data:image/jpeg;base64,/w=="));
    }

    #[test]
    fn rejects_svg_unknown_and_oversized_content() {
        let images = [
            InlineImage::new("svg", "image/svg+xml", b"<svg/>".to_vec()),
            InlineImage::new("unknown", "application/octet-stream", vec![1]),
            InlineImage::new("large", "image/png", vec![0; MAX_INLINE_IMAGE_BYTES + 1]),
        ];
        let html = r#"<img src="cid:svg"><img src="cid:unknown"><img src="cid:large">"#;

        assert_eq!(resolve_cid_images(html, &images), html);
    }
}
