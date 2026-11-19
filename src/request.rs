//! Request values from the page: HTTP, cURL and `.http` output, and the Spring
//! server address from `application.properties` / `application.yml`.

use crate::parser::Endpoint;
use openidea_plugin::{Error, HttpRequest, Project};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Spec {
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub headers: Vec<Header>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub body_base64: Option<String>,
    #[serde(default)]
    pub multipart: Vec<MultipartPart>,
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MultipartPart {
    pub name: String,
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub file: bool,
    #[serde(default)]
    pub filename: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Header {
    pub name: String,
    pub value: String,
}

impl Header {
    fn pair(&self) -> (String, String) {
        (self.name.trim().to_owned(), self.value.clone())
    }
}

const METHODS: [&str; 9] = [
    "GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS", "TRACE", "CONNECT",
];
const MAX_BODY: usize = 8 << 20;
/// Cool Request's default request timeout.
pub const DEFAULT_TIMEOUT_MS: u64 = 10_000;
/// Upper bound for a user-set timeout. Debug sessions use a much larger value
/// so a paused breakpoint does not abort the request.
pub const MAX_TIMEOUT_MS: u64 = 86_400_000;

pub fn to_http(spec: &Spec, project: Option<&Project>) -> Result<HttpRequest, Error> {
    let method = spec.method.trim().to_ascii_uppercase();
    if !METHODS.contains(&method.as_str()) {
        return Err(Error::Invalid(format!("unsupported method {method}")));
    }
    let url = spec.url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(Error::Invalid(
            "the URL must start with http:// or https://".into(),
        ));
    }
    if spec.body.as_ref().is_some_and(|body| body.len() > MAX_BODY) {
        return Err(Error::Invalid(
            "the request body is larger than 8 MiB".into(),
        ));
    }
    let mut headers: Vec<(String, String)> = spec
        .headers
        .iter()
        .filter(|header| !header.name.trim().is_empty())
        .map(Header::pair)
        .collect();
    let mut body = spec.body.as_ref().map(|body| body.as_bytes().to_vec());
    if let Some(encoded) = spec.body_base64.as_deref() {
        let bytes = decode_base64(encoded)?;
        if bytes.len() > MAX_BODY {
            return Err(Error::Invalid(
                "the request body is larger than 8 MiB".into(),
            ));
        }
        body = Some(bytes);
    }
    if !spec.multipart.is_empty() {
        let boundary = format!("----OpenIdeaBoundary{}", multipart_stamp());
        let (bytes, content_type) = multipart_body(&spec.multipart, project, &boundary)?;
        if bytes.len() > MAX_BODY {
            return Err(Error::Invalid(
                "the multipart body is larger than 8 MiB".into(),
            ));
        }
        headers.retain(|(name, _)| !name.eq_ignore_ascii_case("content-type"));
        headers.push(("Content-Type".into(), content_type));
        body = Some(bytes);
    }
    Ok(HttpRequest {
        method,
        url: url.to_owned(),
        headers,
        body,
        timeout_ms: Some(
            spec.timeout_ms
                .unwrap_or(DEFAULT_TIMEOUT_MS)
                .clamp(100, MAX_TIMEOUT_MS) as u32,
        ),
    })
}

fn decode_base64(text: &str) -> Result<Vec<u8>, Error> {
    let mut out = Vec::with_capacity(text.len() * 3 / 4);
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for ch in text.bytes() {
        let value = match ch {
            b'A'..=b'Z' => u32::from(ch - b'A'),
            b'a'..=b'z' => u32::from(ch - b'a') + 26,
            b'0'..=b'9' => u32::from(ch - b'0') + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' | b'\t' => continue,
            _ => return Err(Error::Invalid("invalid base64 request body".into())),
        };
        buffer = (buffer << 6) | value;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
            buffer &= (1 << bits) - 1;
        }
    }
    Ok(out)
}

fn multipart_body(
    parts: &[MultipartPart],
    project: Option<&Project>,
    boundary: &str,
) -> Result<(Vec<u8>, String), Error> {
    let mut out = Vec::new();
    for part in parts.iter().filter(|part| !part.name.trim().is_empty()) {
        let name = escape_quoted(part.name.trim());
        out.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        if part.file {
            let Some(project) = project else {
                return Err(Error::Failed("Open a project to read upload files.".into()));
            };
            let bytes = project.read_bytes(&part.value)?;
            let filename = part
                .filename
                .clone()
                .filter(|name| !name.is_empty())
                .unwrap_or_else(|| basename(&part.value));
            out.extend_from_slice(
                format!(
                    "Content-Disposition: form-data; name=\"{name}\"; filename=\"{}\"\r\n",
                    escape_quoted(&filename)
                )
                .as_bytes(),
            );
            out.extend_from_slice(
                format!(
                    "Content-Type: {}\r\n\r\n",
                    part.content_type
                        .as_deref()
                        .unwrap_or("application/octet-stream")
                )
                .as_bytes(),
            );
            out.extend_from_slice(&bytes);
            out.extend_from_slice(b"\r\n");
        } else {
            out.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n").as_bytes(),
            );
            if let Some(content_type) = part
                .content_type
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                out.extend_from_slice(format!("Content-Type: {content_type}\r\n").as_bytes());
            }
            out.extend_from_slice(b"\r\n");
            out.extend_from_slice(part.value.as_bytes());
            out.extend_from_slice(b"\r\n");
        }
    }
    out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Ok((out, format!("multipart/form-data; boundary={boundary}")))
}

fn multipart_stamp() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn basename(path: &str) -> String {
    path.rsplit(['/', '\\']).next().unwrap_or("file").to_owned()
}

fn escape_quoted(value: &str) -> String {
    value.replace(['"', '\r', '\n'], "_")
}

pub fn curl(spec: &Spec) -> Result<String, Error> {
    let method = spec.method.trim().to_ascii_uppercase();
    let mut out = format!("curl -X {method} {}", quote(spec.url.trim()));
    let mut headers: Vec<Header> = spec
        .headers
        .iter()
        .filter(|header| !header.name.trim().is_empty())
        .cloned()
        .collect();
    if spec.body.as_ref().is_some_and(|body| !body.is_empty())
        && !has_header(&headers, "content-type")
    {
        headers.push(Header {
            name: "Content-Type".into(),
            value: "application/json".into(),
        });
    }
    for header in headers {
        out.push_str(&format!(
            " -H {}",
            quote(&format!("{}: {}", header.name.trim(), header.value))
        ));
    }
    if !spec.multipart.is_empty() {
        for part in spec
            .multipart
            .iter()
            .filter(|part| !part.name.trim().is_empty())
        {
            let mut value = part.value.clone();
            if part.file {
                value = format!("@{}", part.value);
            }
            if let Some(filename) = part.filename.as_deref().filter(|name| !name.is_empty()) {
                value.push_str(&format!(";filename={filename}"));
            }
            if let Some(content_type) = part
                .content_type
                .as_deref()
                .filter(|value| !value.is_empty())
            {
                value.push_str(&format!(";type={content_type}"));
            }
            out.push_str(&format!(
                " -F {}",
                quote(&format!("{}={value}", part.name.trim()))
            ));
        }
        return Ok(out);
    }
    if let Some(body) = spec.body.as_ref().filter(|body| !body.is_empty()) {
        out.push_str(&format!(" --data-raw {}", quote(body)));
    }
    Ok(out)
}

/// Parses the common cURL flags into a request. `-F`/`--form` is rejected until
/// multipart bodies are supported.
pub fn parse_curl(text: &str) -> Result<Spec, Error> {
    let tokens = curl_tokens(text);
    if tokens.is_empty() {
        return Err(Error::Invalid("Paste a cURL command first.".into()));
    }
    let mut index = usize::from(tokens[0].eq_ignore_ascii_case("curl"));
    let mut method: Option<String> = None;
    let mut url: Option<String> = None;
    let mut headers: Vec<Header> = Vec::new();
    let mut data: Vec<String> = Vec::new();
    let mut multipart: Vec<MultipartPart> = Vec::new();
    let mut json_body = false;
    let mut as_get = false;
    while index < tokens.len() {
        let token = &tokens[index];
        let (flag, inline) = split_curl_flag(token);
        let value = |index: &mut usize| next_curl_value(&tokens, index, inline);
        match flag {
            "-X" | "--request" => method = Some(value(&mut index)?.to_ascii_uppercase()),
            "-H" | "--header" => {
                let raw = value(&mut index)?;
                if let Some((name, value)) = raw.split_once(':') {
                    let name = name.trim();
                    if !name.is_empty() {
                        headers.push(Header {
                            name: name.to_owned(),
                            value: value.trim().to_owned(),
                        });
                    }
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-ascii" => {
                data.push(value(&mut index)?);
            }
            "--data-urlencode" => {
                let raw = value(&mut index)?;
                match raw.split_once('=') {
                    Some((name, value)) => data.push(format!("{name}={}", percent_encode(value))),
                    None => data.push(percent_encode(&raw)),
                }
            }
            "--json" => {
                data.push(value(&mut index)?);
                json_body = true;
                method = Some("POST".into());
            }
            "-u" | "--user" => {
                let raw = value(&mut index)?;
                headers.push(Header {
                    name: "Authorization".into(),
                    value: format!("Basic {}", base64(raw.as_bytes())),
                });
            }
            "-b" | "--cookie" => headers.push(Header {
                name: "Cookie".into(),
                value: value(&mut index)?,
            }),
            "-A" | "--user-agent" => {
                headers.push(Header {
                    name: "User-Agent".into(),
                    value: value(&mut index)?,
                });
            }
            "-e" | "--referer" => headers.push(Header {
                name: "Referer".into(),
                value: value(&mut index)?,
            }),
            "--url" => url = Some(value(&mut index)?),
            "-G" | "--get" => as_get = true,
            "-I" | "--head" => method = Some("HEAD".into()),
            "-F" | "--form" => {
                let raw = value(&mut index)?;
                let Some((name, rest)) = raw.split_once('=') else {
                    return Err(Error::Invalid("A -F field needs name=value.".into()));
                };
                let mut part = MultipartPart {
                    name: name.to_owned(),
                    value: rest.to_owned(),
                    file: false,
                    filename: None,
                    content_type: None,
                };
                if let Some(path) = rest.strip_prefix('@') {
                    part.file = true;
                    let mut pieces = path.split(';');
                    part.value = pieces.next().unwrap_or_default().to_owned();
                    for piece in pieces {
                        if let Some(filename) = piece.strip_prefix("filename=") {
                            part.filename = Some(filename.to_owned());
                        } else if let Some(content_type) = piece.strip_prefix("type=") {
                            part.content_type = Some(content_type.to_owned());
                        }
                    }
                }
                multipart.push(part);
            }
            "-o" | "--output" | "-x" | "--proxy" | "--cacert" | "--cert" | "--key"
            | "--resolve" | "--connect-timeout" | "--max-time" | "--retry" => {
                let _ = value(&mut index)?;
            }
            "--compressed" | "-s" | "--silent" | "-S" | "--show-error" | "-k" | "--insecure"
            | "-L" | "--location" | "-i" | "--include" | "-v" | "--verbose" | "-f" | "--fail"
            | "--globoff" | "-g" | "--no-buffer" | "-N" => {}
            _ if token.starts_with('-') => {
                return Err(Error::Invalid(format!("Unsupported cURL option: {token}")));
            }
            _ => {
                if url.is_some() {
                    return Err(Error::Invalid(
                        "The cURL command has more than one URL.".into(),
                    ));
                }
                url = Some(token.clone());
            }
        }
        index += 1;
    }
    let mut url = url.ok_or_else(|| Error::Invalid("The cURL command has no URL.".into()))?;
    let body = if data.is_empty() {
        None
    } else {
        Some(data.join("&"))
    };
    let method = method.unwrap_or_else(|| {
        if as_get || (body.is_none() && multipart.is_empty()) {
            "GET".into()
        } else {
            "POST".into()
        }
    });
    if as_get && let Some(body) = body.as_deref() {
        let separator = if url.contains('?') { '&' } else { '?' };
        url.push(separator);
        url.push_str(body);
    }
    let body = if as_get { None } else { body };
    if body.is_some() && !has_header(&headers, "content-type") {
        let looks_json = body
            .as_deref()
            .is_some_and(|body| body.trim_start().starts_with(['{', '[']));
        headers.push(Header {
            name: "Content-Type".into(),
            value: if json_body || looks_json {
                "application/json".into()
            } else {
                "application/x-www-form-urlencoded".into()
            },
        });
    }
    Ok(Spec {
        method,
        url,
        headers,
        body,
        body_base64: None,
        multipart,
        timeout_ms: None,
    })
}

fn split_curl_flag(token: &str) -> (&str, Option<&str>) {
    if let Some(rest) = token.strip_prefix("--") {
        if let Some((flag, value)) = rest.split_once('=') {
            return (&token[..flag.len() + 2], Some(value));
        }
        return (token, None);
    }
    if token.starts_with('-') && token.len() > 2 && !token.starts_with("--") {
        return (&token[..2], Some(&token[2..]));
    }
    (token, None)
}

fn next_curl_value(
    tokens: &[String],
    index: &mut usize,
    inline: Option<&str>,
) -> Result<String, Error> {
    if let Some(value) = inline {
        return Ok(value.to_owned());
    }
    *index += 1;
    tokens
        .get(*index)
        .cloned()
        .ok_or_else(|| Error::Invalid("A cURL option is missing its value.".into()))
}

fn curl_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\\' => match chars.peek().copied() {
                Some('\n') => {
                    chars.next();
                }
                Some('\r') => {
                    chars.next();
                    if chars.peek() == Some(&'\n') {
                        chars.next();
                    }
                }
                Some(next) => {
                    chars.next();
                    current.push(next);
                }
                None => {}
            },
            '\'' => {
                for next in chars.by_ref() {
                    if next == '\'' {
                        break;
                    }
                    current.push(next);
                }
            }
            '"' => {
                while let Some(next) = chars.next() {
                    match next {
                        '"' => break,
                        '\\' => {
                            if let Some(escaped) = chars.next() {
                                current.push(match escaped {
                                    'n' => '\n',
                                    'r' => '\r',
                                    't' => '\t',
                                    other => other,
                                });
                            }
                        }
                        other => current.push(other),
                    }
                }
            }
            ch if ch.is_whitespace() => {
                if !current.is_empty() {
                    out.push(std::mem::take(&mut current));
                }
            }
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

fn percent_encode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

/// Standard base64, used for Basic auth and binary response previews.
pub(crate) fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(TABLE[(b0 >> 2) as usize] as char);
        out.push(TABLE[(((b0 & 0x03) << 4) | (b1 >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((b1 & 0x0f) << 2) | (b2 >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(b2 & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

pub fn http_file(spec: &Spec, name: &str) -> Result<String, Error> {
    if !spec.multipart.is_empty() {
        return Err(Error::Invalid(
            "multipart requests cannot be saved as .http yet.".into(),
        ));
    }
    let method = spec.method.trim().to_ascii_uppercase();
    let mut headers: Vec<Header> = spec
        .headers
        .iter()
        .filter(|header| !header.name.trim().is_empty())
        .cloned()
        .collect();
    let body = spec.body.as_deref().filter(|body| !body.is_empty());
    if body.is_some() && !has_header(&headers, "content-type") {
        headers.push(Header {
            name: "Content-Type".into(),
            value: "application/json".into(),
        });
    }
    let mut lines = vec![
        format!("### {name}"),
        format!("{method} {}", spec.url.trim()),
    ];
    for header in headers {
        lines.push(format!("{}: {}", header.name.trim(), header.value));
    }
    let mut out = lines.join("\n");
    if let Some(body) = body {
        out.push_str("\n\n");
        out.push_str(body.trim_end());
    }
    out.push('\n');
    Ok(out)
}

fn has_header(headers: &[Header], name: &str) -> bool {
    headers
        .iter()
        .any(|header| header.name.trim().eq_ignore_ascii_case(name))
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// A file name that is safe under `.openidea/api-client`.
pub fn slug(name: &str) -> String {
    let mut out = String::new();
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
            out.push(ch.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches(['-', '.']).to_owned();
    if trimmed.is_empty() {
        "request".into()
    } else {
        trimmed
    }
}

/// `http://localhost:<server.port><context-path>` for `source`, from the nearest
/// Spring Boot configuration file; `http://localhost:8080` without one.
pub fn host_for(project: &Project, source: &str) -> String {
    let root = project.root().unwrap_or_default();
    let module = module_root(source);
    let mut dirs: Vec<&str> = Vec::new();
    if let Some(module) = &module {
        dirs.push(module);
    }
    if !root.is_empty() && !dirs.contains(&root.as_str()) {
        dirs.push(&root);
    }

    // Standard Spring Boot locations, nearest module first, then the project root.
    let mut values = std::collections::BTreeMap::new();
    for dir in &dirs {
        for path in config_paths(dir) {
            merge_config(project, &path, &mut values, false);
        }
    }

    // Non-standard locations (config/application.yml, a sibling module, …).
    let mut extra = Vec::new();
    for pattern in [
        "**/application.properties",
        "**/application.yml",
        "**/application.yaml",
    ] {
        if let Ok(found) = project.find_files(pattern, 100) {
            extra.extend(found);
        }
    }
    extra.retain(|path| !path.contains("/target/") && !path.contains("/build/"));
    extra.sort_by_key(|path| std::cmp::Reverse(common_prefix(path, source)));
    extra.dedup();
    for path in extra {
        merge_config(project, &path, &mut values, false);
    }

    // application-<profile>.* overrides the base files.
    if let Some(active) = values.get("spring.profiles.active").cloned() {
        for profile in active
            .split(',')
            .map(str::trim)
            .filter(|profile| !profile.is_empty())
        {
            for dir in &dirs {
                for path in profile_config_paths(dir, profile) {
                    merge_config(project, &path, &mut values, true);
                }
            }
        }
    }

    host_from_values(&values)
}

pub(crate) fn module_root(source: &str) -> Option<String> {
    let source = source.replace('\\', "/");
    let at = source.find("/src/main/")?;
    Some(source[..at].to_owned())
}

fn config_paths(dir: &str) -> Vec<String> {
    let dir = dir.trim_end_matches(['/', '\\']);
    let mut out = Vec::new();
    for name in [
        "application.properties",
        "application.yml",
        "application.yaml",
    ] {
        out.push(format!("{dir}/src/main/resources/{name}"));
    }
    for name in [
        "application.properties",
        "application.yml",
        "application.yaml",
    ] {
        out.push(format!("{dir}/{name}"));
    }
    out
}

fn profile_config_paths(dir: &str, profile: &str) -> Vec<String> {
    let dir = dir.trim_end_matches(['/', '\\']);
    let mut out = Vec::new();
    for name in ["properties", "yml", "yaml"] {
        out.push(format!(
            "{dir}/src/main/resources/application-{profile}.{name}"
        ));
    }
    out
}

fn merge_config(
    project: &Project,
    path: &str,
    values: &mut std::collections::BTreeMap<String, String>,
    overwrite: bool,
) {
    let Ok(text) = project.read_file(path) else {
        return;
    };
    let parsed = if path.ends_with(".properties") {
        parse_properties(&text)
    } else {
        parse_yaml(&text)
    };
    for (key, value) in parsed {
        if overwrite {
            values.insert(key, value);
        } else {
            values.entry(key).or_insert(value);
        }
    }
}

fn host_from_values(values: &std::collections::BTreeMap<String, String>) -> String {
    let port = values
        .get("server.port")
        .map(|value| config_value(value))
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|port| *port != 0)
        .unwrap_or(8080);
    let ssl = values
        .get("server.ssl.enabled")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
        || values.contains_key("server.ssl.key-store");
    let context = [
        values.get("server.servlet.context-path"),
        values.get("server.context-path"),
        values.get("spring.webflux.base-path"),
    ]
    .into_iter()
    .flatten()
    .map(|value| config_value(value))
    .next()
    .unwrap_or_default();
    let context = context.trim().trim_matches('/');
    let scheme = if ssl { "https" } else { "http" };
    if context.is_empty() {
        format!("{scheme}://localhost:{port}")
    } else {
        format!("{scheme}://localhost:{port}/{context}")
    }
}

/// `${PORT:8081}` → `8081`, and surrounding quotes removed.
fn config_value(value: &str) -> String {
    let value = unquote(value.trim());
    if let Some(inner) = value
        .strip_prefix("${")
        .and_then(|value| value.strip_suffix('}'))
    {
        if let Some((_, default)) = inner.split_once(':') {
            return default.trim().to_owned();
        }
    }
    value
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(a, b)| a == b).count()
}

pub fn parse_properties(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((key, value)) = line.split_once('=').or_else(|| line.split_once(':')) else {
            continue;
        };
        out.insert(key.trim().to_owned(), unquote(value.trim()));
    }
    out
}

pub fn parse_yaml(text: &str) -> std::collections::BTreeMap<String, String> {
    let mut out = std::collections::BTreeMap::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    for raw in text.lines() {
        if raw.trim_start().starts_with('#') {
            continue;
        }
        let line = match raw.split_once(" #") {
            Some((line, _)) => line,
            None => raw,
        };
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with("---") || trimmed.starts_with("- ") {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let indent = line.len() - line.trim_start().len();
        while stack.last().is_some_and(|(level, _)| *level >= indent) {
            stack.pop();
        }
        let key = key.trim().trim_matches(['"', '\'']);
        let value = value.trim();
        let path = stack
            .iter()
            .map(|(_, key)| key.as_str())
            .chain(std::iter::once(key))
            .collect::<Vec<_>>()
            .join(".");
        if value.is_empty() {
            stack.push((indent, key.to_owned()));
            continue;
        }
        out.insert(path, unquote(value));
    }
    out
}

fn unquote(value: &str) -> String {
    value.trim().trim_matches(['"', '\'']).to_owned()
}

/// The `{name}` variables of a mapping path.
pub fn path_params(path: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'{' && bytes.get(i + 1) != Some(&b'{') {
            let start = i + 1;
            if let Some(end) = path[start..].find('}') {
                let name = &path[start..start + end];
                let name = name
                    .split(':')
                    .next()
                    .unwrap_or(name)
                    .trim_start_matches('*')
                    .trim();
                if !name.is_empty() && !out.iter().any(|n| n == name) {
                    out.push(name.to_owned());
                }
                i = start + end + 1;
                continue;
            }
        }
        i += 1;
    }
    out
}

#[allow(dead_code)]
pub fn endpoint_name(endpoint: &Endpoint) -> String {
    format!("{}.{}", endpoint.controller, endpoint.method_name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> Spec {
        Spec {
            multipart: Vec::new(),
            timeout_ms: None,
            method: "post".into(),
            url: "http://localhost:8080/api/users".into(),
            headers: vec![Header {
                name: "Accept".into(),
                value: "application/json".into(),
            }],
            body: Some(r#"{"name":"Ada"}"#.into()),
            body_base64: None,
        }
    }

    #[test]
    fn validates_and_builds_an_http_request() {
        let request = to_http(&spec(), None).unwrap();
        assert_eq!(request.method, "POST");
        assert_eq!(
            request.headers,
            [("Accept".into(), "application/json".into())]
        );
        assert_eq!(
            request.body.as_deref(),
            Some(br#"{"name":"Ada"}"#.as_slice())
        );
        assert!(
            to_http(
                &Spec {
                    url: "ftp://x".into(),
                    ..spec()
                },
                None
            )
            .is_err()
        );
        assert!(
            to_http(
                &Spec {
                    method: "TRACE".into(),
                    ..spec()
                },
                None
            )
            .is_ok()
        );
    }

    #[test]
    fn renders_curl_and_http() {
        let curl = curl(&spec()).unwrap();
        assert!(curl.starts_with("curl -X POST 'http://localhost:8080/api/users'"));
        assert!(curl.contains("-H 'Accept: application/json'"));
        assert!(curl.contains("-H 'Content-Type: application/json'"));
        assert!(curl.contains("--data-raw '{\"name\":\"Ada\"}'"));

        let file = http_file(&spec(), "UserController.create").unwrap();
        assert!(
            file.starts_with("### UserController.create\nPOST http://localhost:8080/api/users")
        );
        assert!(file.contains("Content-Type: application/json"));
        assert!(file.ends_with("\n{\"name\":\"Ada\"}\n"));
    }

    #[test]
    fn reads_the_nearest_module_configuration() {
        let host = openidea_plugin::mock::MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file("/p/src/main/resources/application.yml", "");
        host.add_file(
            "/p/module/src/main/resources/application.yml",
            "server:\n  port: 8088\n",
        );
        let ctx = host.invocation();
        let project = ctx.project().unwrap();
        assert_eq!(
            host_for(&project, "/p/module/src/main/java/x/Controller.java"),
            "http://localhost:8088"
        );
    }

    #[test]
    fn builds_the_host_from_values() {
        let mut values = std::collections::BTreeMap::new();
        values.insert("server.port".into(), "8088".into());
        assert_eq!(host_from_values(&values), "http://localhost:8088");
        values.insert("server.servlet.context-path".into(), "/api".into());
        assert_eq!(host_from_values(&values), "http://localhost:8088/api");
        values.insert("server.ssl.enabled".into(), "true".into());
        assert_eq!(host_from_values(&values), "https://localhost:8088/api");
        values.insert("server.port".into(), "${PORT:9090}".into());
        assert_eq!(host_from_values(&values), "https://localhost:9090/api");
    }

    #[test]
    fn finds_the_module_root_and_standard_configs() {
        assert_eq!(
            module_root("/p/module/src/main/java/x/C.java").as_deref(),
            Some("/p/module")
        );
        assert_eq!(module_root("/p/C.java"), None);
        assert_eq!(
            config_paths("/p/module")[0],
            "/p/module/src/main/resources/application.properties"
        );
        assert_eq!(
            profile_config_paths("/p/module", "dev")[1],
            "/p/module/src/main/resources/application-dev.yml"
        );
    }

    #[test]
    fn imports_common_curl_commands() {
        let spec = parse_curl(
            r#"curl -X POST 'http://localhost:8088/address/add' -H 'Content-Type: application/json' -H "X-Test: a b" --data-raw '{"city":"SH"}'"#,
        )
        .unwrap();
        assert_eq!(spec.method, "POST");
        assert_eq!(spec.url, "http://localhost:8088/address/add");
        assert_eq!(spec.body.as_deref(), Some(r#"{"city":"SH"}"#));
        assert!(
            spec.headers
                .iter()
                .any(|header| header.name == "X-Test" && header.value == "a b")
        );
        assert!(
            spec.headers
                .iter()
                .any(|header| header.name == "Content-Type")
        );
        let get = parse_curl("curl --url https://example.com/api?q=1").unwrap();
        assert_eq!(get.method, "GET");
        assert_eq!(get.url, "https://example.com/api?q=1");
        let auth = parse_curl("curl -u user:pass http://localhost:8080/x").unwrap();
        assert_eq!(auth.headers[0].value, "Basic dXNlcjpwYXNz");
    }

    #[test]
    fn imports_curl_line_continuations() {
        let spec = parse_curl(
            "curl 'http://localhost:8080/x' \\\n  -H 'Accept: application/json' \\\n  --data 'a=1'",
        )
        .unwrap();
        assert_eq!(spec.method, "POST");
        assert_eq!(spec.body.as_deref(), Some("a=1"));
        assert_eq!(spec.headers[0].name, "Accept");
    }

    #[test]
    fn decodes_a_binary_body() {
        let spec = Spec {
            body: None,
            body_base64: Some("AQIDBA==".into()),
            ..spec()
        };
        let request = to_http(&spec, None).unwrap();
        assert_eq!(request.body.as_deref(), Some(&[1, 2, 3, 4][..]));
    }
    #[test]
    fn builds_multipart_text_and_file_parts() {
        let host = openidea_plugin::mock::MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file("/p/logo.txt", "hello");
        let project = host.invocation().project().unwrap();
        let spec = Spec {
            method: "POST".into(),
            url: "http://localhost:8080/upload".into(),
            headers: Vec::new(),
            body: None,
            body_base64: None,
            timeout_ms: None,
            multipart: vec![
                MultipartPart {
                    name: "name".into(),
                    value: "Ada".into(),
                    file: false,
                    filename: None,
                    content_type: None,
                },
                MultipartPart {
                    name: "avatar".into(),
                    value: "/p/logo.txt".into(),
                    file: true,
                    filename: None,
                    content_type: Some("text/plain".into()),
                },
            ],
        };
        let request = to_http(&spec, Some(&project)).unwrap();
        let body = String::from_utf8_lossy(request.body.as_deref().unwrap());
        assert!(body.contains("name=\"name\""));
        assert!(body.contains("Ada"));
        assert!(body.contains("filename=\"logo.txt\""));
        assert!(body.contains("hello"));
        assert!(
            request
                .headers
                .iter()
                .any(|(name, value)| name == "Content-Type"
                    && value.starts_with("multipart/form-data; boundary="))
        );
    }

    #[test]
    fn imports_multipart_curl_fields() {
        let spec = parse_curl(
            "curl -F 'name=Ada' -F 'avatar=@/p/logo.png;type=image/png' http://localhost:8080/upload",
        )
        .unwrap();
        assert_eq!(spec.method, "POST");
        assert_eq!(spec.multipart.len(), 2);
        assert!(!spec.multipart[0].file);
        assert!(spec.multipart[1].file);
        assert_eq!(spec.multipart[1].value, "/p/logo.png");
        assert_eq!(spec.multipart[1].content_type.as_deref(), Some("image/png"));
    }

    #[test]
    fn reads_configuration_and_paths() {
        let properties = parse_properties(
            "server.port=9090\nserver.servlet.context-path=/api\nserver.ssl.enabled=true\n",
        );
        assert_eq!(
            properties.get("server.port").map(String::as_str),
            Some("9090")
        );
        assert_eq!(
            properties
                .get("server.servlet.context-path")
                .map(String::as_str),
            Some("/api")
        );

        let yaml = parse_yaml("server:\n  port: 8181\n  servlet:\n    context-path: /v1\n");
        assert_eq!(yaml.get("server.port").map(String::as_str), Some("8181"));
        assert_eq!(
            yaml.get("server.servlet.context-path").map(String::as_str),
            Some("/v1")
        );

        assert_eq!(
            path_params("/api/{id}/posts/{postId:[0-9]+}"),
            ["id", "postId"]
        );
        assert_eq!(slug("User Controller.create!"), "user-controller-create");
    }
}
