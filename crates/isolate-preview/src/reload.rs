//! Preview-only browser refresh. Never embeds deployment credentials or source code.
pub const SCRIPT: &[u8] = include_bytes!("../browser/reload.js");

pub fn inject(status: u16, mime: &str, mut body: Vec<u8>, revision: &str) -> Vec<u8> {
    // Do not corrupt JSON, binary data, or pages with an unsupported encoding.
    let mut parts = mime.split(';');
    let html = parts
        .next()
        .is_some_and(|p| p.trim().eq_ignore_ascii_case("text/html"));
    let supported_encoding = parts.all(|p| {
        let Some((name, value)) = p.trim().split_once('=') else {
            return true;
        };
        !name.trim().eq_ignore_ascii_case("charset")
            || matches!(
                value.trim().trim_matches('"').to_ascii_lowercase().as_str(),
                "utf-8" | "utf8" | "us-ascii"
            )
    });
    if status == 200 && html && supported_encoding && std::str::from_utf8(&body).is_ok() {
        // Appending preserves the doctype. Pin to the worker snapshot that produced this document,
        // not to whichever revision happens to be live when the script or first poll is fetched.
        body.extend_from_slice(
            format!("\n<script src=\"/__spki/v1/reload.js?revision={revision}\" defer></script>\n")
                .as_bytes(),
        );
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn injects_only_successful_utf8_html_and_preserves_doctype() {
        let html = b"<!doctype html><html><body>Hello</body></html>".to_vec();
        let result = inject(
            200,
            "Text/HTML; charset=UTF-8",
            html.clone(),
            &"a".repeat(64),
        );
        assert!(result.starts_with(&html));
        assert!(
            String::from_utf8(result)
                .unwrap()
                .contains(&format!("revision={}", "a".repeat(64)))
        );
        for (status, mime) in [
            (500, "text/html"),
            (200, "application/json"),
            (200, "text/html;charset=utf-16"),
        ] {
            assert_eq!(inject(status, mime, html.clone(), "unused"), html);
        }
        assert_eq!(inject(200, "text/html", vec![255], "unused"), vec![255]);
    }
}
