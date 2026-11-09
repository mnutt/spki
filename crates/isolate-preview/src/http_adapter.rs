//! Translation of the HTTP subset supported by Sandstorm WebSession.
//!
//! Buffered GET/HEAD/POST/PUT/PATCH/DELETE are supported. Streaming uploads fall back
//! to buffered requests (9 MiB maximum); worker responses are bounded to 1 MiB.
//! WebSockets, WebDAV and OPTIONS are not implemented. Cache policy remains no-store.
//! Only schema-whitelisted additional headers are forwarded. Security headers such
//! as CSP are controlled by Sandstorm, not an unrestricted HTTP proxy. Cookies use
//! Sandstorm's grain scoping and Secure policy; Domain/SameSite have no wire fields.
use crate::transport::{HttpResponse, valid_header};
use isolate_protocol::web_session_capnp::web_session as web;

fn allowed(name: &str, whitelist: capnp::text_list::Reader<'_>) -> capnp::Result<bool> {
    for pattern in whitelist {
        let pattern = pattern?.to_str()?;
        if pattern
            .strip_suffix('*')
            .map_or(name == pattern, |prefix| name.starts_with(prefix))
        {
            return Ok(true);
        }
    }
    Ok(false)
}
fn tag(tag: web::e_tag::Reader<'_>) -> capnp::Result<String> {
    Ok(format!(
        "{}\"{}\"",
        if tag.get_weak() { "W/" } else { "" },
        tag.get_value()?.to_str()?
    ))
}
pub(crate) fn request_headers(
    context: web::context::Reader<'_>,
) -> capnp::Result<Vec<(String, String)>> {
    let mut headers = Vec::new();
    for header in context.get_additional_headers()? {
        let name = header.get_name()?.to_str()?.to_ascii_lowercase();
        let value = header.get_value()?.to_str()?;
        // In particular, request-controlled fields cannot overwrite authenticated identity.
        if allowed(&name, web::context::HEADER_WHITELIST.get()?)? && valid_header(&name, value) {
            headers.push((name, value.to_owned()));
        }
    }
    let mut cookies = Vec::new();
    for cookie in context.get_cookies()? {
        let name = cookie.get_key()?.to_str()?;
        let value = cookie.get_value()?.to_str()?;
        if cookie_part(name) && !name.contains('=') && cookie_part(value) {
            cookies.push(format!("{name}={value}"));
        }
    }
    if !cookies.is_empty() {
        headers.push(("cookie".into(), cookies.join("; ")));
    }
    let mut accept = Vec::new();
    for item in context.get_accept()? {
        accept.push(format!(
            "{};q={}",
            item.get_mime_type()?.to_str()?,
            item.get_q_value()
        ));
    }
    if !accept.is_empty() {
        headers.push(("accept".into(), accept.join(", ")));
    }
    let mut encodings = Vec::new();
    for item in context.get_accept_encoding()? {
        encodings.push(format!(
            "{};q={}",
            item.get_content_coding()?.to_str()?,
            item.get_q_value()
        ));
    }
    if !encodings.is_empty() {
        headers.push(("accept-encoding".into(), encodings.join(", ")));
    }
    use web::context::e_tag_precondition::Which;
    match context.get_e_tag_precondition().which()? {
        Which::None(()) => (),
        Which::Exists(()) => headers.push(("if-match".into(), "*".into())),
        Which::DoesntExist(()) => headers.push(("if-none-match".into(), "*".into())),
        Which::MatchesOneOf(tags) => headers.push((
            "if-match".into(),
            tags?
                .iter()
                .map(tag)
                .collect::<capnp::Result<Vec<_>>>()?
                .join(", "),
        )),
        Which::MatchesNoneOf(tags) => headers.push((
            "if-none-match".into(),
            tags?
                .iter()
                .map(tag)
                .collect::<capnp::Result<Vec<_>>>()?
                .join(", "),
        )),
    }
    Ok(headers)
}
fn cookie_part(value: &str) -> bool {
    !value.contains([';', ',', '\r', '\n']) && value.bytes().all(|b| b >= 32 && b != 127)
}
fn set_cookie(text: &str, mut output: web::cookie::Builder<'_>) -> capnp::Result<()> {
    let mut parts = text.split(';').map(str::trim);
    let (name, value) = parts
        .next()
        .unwrap_or("")
        .split_once('=')
        .ok_or_else(|| capnp::Error::failed("invalid Set-Cookie".into()))?;
    if name.is_empty() || !cookie_part(name) || !cookie_part(value) {
        return Err(capnp::Error::failed("invalid Set-Cookie".into()));
    }
    output.set_name(name);
    output.set_value(value);
    let mut max_age = None;
    let mut expires = None;
    for part in parts {
        let (name, value) = part.split_once('=').unwrap_or((part, ""));
        match name.trim().to_ascii_lowercase().as_str() {
            "httponly" => output.set_http_only(true),
            "path" if cookie_part(value) => output.set_path(value),
            "max-age" => max_age = value.parse::<i64>().ok(),
            "expires" => expires = httpdate::parse_http_date(value).ok(),
            // Sandstorm scopes cookies to the grain and forces Secure. Domain and SameSite
            // are not representable by this protocol; the platform controls their policy.
            _ => (),
        }
    }
    if let Some(seconds) = max_age {
        if seconds <= 0 {
            output.init_expires().set_absolute(0);
        } else {
            output.init_expires().set_relative(seconds as u64);
        }
    } else if let Some(time) = expires {
        let seconds = match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => duration.as_secs() as i64,
            Err(error) => -(error.duration().as_secs() as i64),
        };
        output.init_expires().set_absolute(seconds);
    }
    Ok(())
}
fn set_tag(value: &str, mut output: web::e_tag::Builder<'_>) {
    let (weak, value) = value
        .strip_prefix("W/")
        .map_or((false, value), |v| (true, v));
    output.set_weak(weak);
    output.set_value(value.trim_matches('"'));
}
pub(crate) fn write_response(
    mut output: web::response::Builder<'_>,
    response: &HttpResponse,
) -> capnp::Result<()> {
    output.reborrow().init_cache_policy(); // Conservative: never cache across identity/revision changes.
    let cookies: Vec<_> = response
        .headers
        .iter()
        .filter(|(n, _)| n == "set-cookie")
        .collect();
    let mut list = output.reborrow().init_set_cookies(cookies.len() as u32);
    for (i, (_, value)) in cookies.iter().enumerate() {
        set_cookie(value, list.reborrow().get(i as u32))?;
    }
    let mut additional = Vec::new();
    for (name, value) in &response.headers {
        if allowed(name, web::response::HEADER_WHITELIST.get()?)? {
            additional.push((name, value));
        }
    }
    let mut list = output
        .reborrow()
        .init_additional_headers(additional.len() as u32);
    for (i, (name, value)) in additional.iter().enumerate() {
        let mut header = list.reborrow().get(i as u32);
        header.set_name(name.as_str());
        header.set_value(value.as_str());
    }
    let mime = match response.header("content-type") {
        "" => "application/octet-stream",
        mime => mime,
    };
    match response.status {
        200 | 201 | 202 | 206 | 207 => {
            use web::response::SuccessCode;
            let mut content = output.init_content();
            content.set_status_code(match response.status {
                201 => SuccessCode::Created,
                202 => SuccessCode::Accepted,
                206 => SuccessCode::PartialContent,
                207 => SuccessCode::MultiStatus,
                _ => SuccessCode::Ok,
            });
            content.set_mime_type(mime);
            content.set_encoding(response.header("content-encoding"));
            content.set_language(response.header("content-language"));
            if !response.header("etag").is_empty() {
                set_tag(response.header("etag"), content.reborrow().init_e_tag());
            }
            if let Some(filename) = response
                .header("content-disposition")
                .strip_prefix("attachment; filename=")
            {
                content
                    .reborrow()
                    .init_disposition()
                    .set_download(filename.trim_matches('"'));
            }
            content.init_body().set_bytes(&response.body);
        }
        204 | 205 => {
            let mut content = output.init_no_content();
            content.set_should_reset_form(response.status == 205);
            if !response.header("etag").is_empty() {
                set_tag(response.header("etag"), content.init_e_tag());
            }
        }
        304 => {
            let content = output.init_precondition_failed();
            if !response.header("etag").is_empty() {
                set_tag(response.header("etag"), content.init_matching_e_tag());
            }
        }
        301 | 302 | 303 | 307 | 308 => {
            let mut redirect = output.init_redirect();
            redirect.set_is_permanent(matches!(response.status, 301 | 308));
            redirect.set_switch_to_get(matches!(response.status, 301..=303));
            redirect.set_location(response.header("location"));
        }
        status => {
            let mut body = if (400..500).contains(&status) {
                use web::response::ClientErrorCode as Code;
                let mut error = output.init_client_error();
                error.set_status_code(match status {
                    403 => Code::Forbidden,
                    404 => Code::NotFound,
                    405 => Code::MethodNotAllowed,
                    406 => Code::NotAcceptable,
                    409 => Code::Conflict,
                    410 => Code::Gone,
                    412 => Code::PreconditionFailed,
                    413 => Code::RequestEntityTooLarge,
                    414 => Code::RequestUriTooLong,
                    415 => Code::UnsupportedMediaType,
                    418 => Code::ImATeapot,
                    422 => Code::UnprocessableEntity,
                    _ => Code::BadRequest,
                });
                error.init_non_html_body()
            } else {
                output.init_server_error().init_non_html_body()
            };
            body.set_mime_type(mime);
            body.set_encoding(response.header("content-encoding"));
            body.set_language(response.header("content-language"));
            body.set_data(&response.body);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn statuses_metadata_and_cookies_survive_translation() -> capnp::Result<()> {
        for status in [
            200, 201, 202, 204, 205, 206, 207, 301, 302, 303, 304, 307, 308, 403, 404, 405, 409,
            413, 422, 500,
        ] {
            let response = HttpResponse {
                status,
                body: b"body".to_vec(),
                headers: vec![
                    ("content-type".into(), "text/plain".into()),
                    ("content-encoding".into(), "gzip".into()),
                    ("etag".into(), "W/\"version\"".into()),
                    ("location".into(), "/next".into()),
                    (
                        "set-cookie".into(),
                        "session=abc; Path=/; HttpOnly; Max-Age=60".into(),
                    ),
                    ("set-cookie".into(), "old=; Max-Age=0".into()),
                    ("content-range".into(), "bytes 0-3/4".into()),
                    ("connection".into(), "close".into()),
                ],
            };
            let mut message = capnp::message::Builder::new_default();
            write_response(message.init_root(), &response)?;
            let reader = message.get_root_as_reader::<web::response::Reader>()?;
            assert_eq!(reader.get_set_cookies()?.len(), 2);
            let cookie = reader.get_set_cookies()?.get(0);
            assert!(cookie.get_http_only());
            assert_eq!(cookie.get_path()?.to_str()?, "/");
            assert!(matches!(
                cookie.get_expires().which()?,
                web::cookie::expires::Relative(60)
            ));
            assert_eq!(reader.get_additional_headers()?.len(), 1);
            use web::response::Which;
            match reader.which()? {
                Which::Content(content) => {
                    assert!(matches!(status, 200 | 201 | 202 | 206 | 207));
                    assert_eq!(content.get_encoding()?.to_str()?, "gzip");
                    assert_eq!(content.get_e_tag()?.get_value()?.to_str()?, "version");
                    assert!(content.get_e_tag()?.get_weak());
                    if status == 201 {
                        assert_eq!(
                            content.get_status_code()?,
                            web::response::SuccessCode::Created
                        );
                    }
                }
                Which::NoContent(content) => {
                    assert!(matches!(status, 204 | 205));
                    assert_eq!(content.get_should_reset_form(), status == 205);
                }
                Which::Redirect(redirect) => {
                    assert!(matches!(status, 301 | 302 | 303 | 307 | 308));
                    assert_eq!(redirect.get_location()?.to_str()?, "/next");
                    assert_eq!(redirect.get_is_permanent(), matches!(status, 301 | 308));
                    assert_eq!(redirect.get_switch_to_get(), matches!(status, 301..=303));
                }
                Which::PreconditionFailed(_) => assert_eq!(status, 304),
                Which::ClientError(error) => {
                    assert!((400..500).contains(&status));
                    if status == 413 {
                        assert_eq!(
                            error.get_status_code()?,
                            web::response::ClientErrorCode::RequestEntityTooLarge
                        );
                    }
                }
                Which::ServerError(_) => assert_eq!(status, 500),
            }
        }
        Ok(())
    }
    #[test]
    fn request_context_preserves_metadata_but_cannot_forge_identity() -> capnp::Result<()> {
        let mut message = capnp::message::Builder::new_default();
        let mut context = message.init_root::<web::context::Builder>();
        let mut cookie = context.reborrow().init_cookies(1).get(0);
        cookie.set_key("session");
        cookie.set_value("abc");
        context.reborrow().init_e_tag_precondition().set_exists(());
        let mut accept = context.reborrow().init_accept(1).get(0);
        accept.set_mime_type("application/json");
        accept.set_q_value(1.0);
        let mut headers = context.init_additional_headers(3);
        for (i, (name, value)) in [
            ("x-sandstorm-user-id", "forged"),
            ("x-sandstorm-app-test", "ok"),
            ("range", "bytes=0-3"),
        ]
        .iter()
        .enumerate()
        {
            let mut header = headers.reborrow().get(i as u32);
            header.set_name(*name);
            header.set_value(*value);
        }
        let headers = request_headers(message.get_root_as_reader()?)?;
        assert!(
            !headers
                .iter()
                .any(|(name, _)| name == "x-sandstorm-user-id")
        );
        for (name, value) in [
            ("cookie", "session=abc"),
            ("if-match", "*"),
            ("range", "bytes=0-3"),
            ("x-sandstorm-app-test", "ok"),
        ] {
            assert!(headers.contains(&(name.into(), value.into())));
        }
        Ok(())
    }
}
