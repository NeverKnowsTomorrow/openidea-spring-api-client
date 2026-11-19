//! A small Spring Web reader for the plugin: controllers, mappings and method parameters.
//!
//! It reads the annotations and signatures it needs from Java and Kotlin source;
//! it is not a compiler. Unusual layouts (annotations on generated code, nested
//! controllers in the same file) may be skipped rather than guessed.

use serde::Serialize;
use std::collections::HashSet;

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Endpoint {
    pub key: String,
    pub source: String,
    pub controller: String,
    pub controller_package: String,
    pub controller_description: String,
    pub description: String,
    pub method_name: String,
    pub http_method: String,
    pub paths: Vec<String>,
    pub path: String,
    pub query: Vec<QueryParam>,
    pub headers: Vec<Header>,
    pub consumes: Option<String>,
    pub produces: Option<String>,
    pub body_type: Option<String>,
    /// Line of the mapping annotation (0-based).
    pub line: u32,
    /// Line of the method name (0-based); the IDE's API gutter icon is here.
    pub method_line: u32,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueryParam {
    pub name: String,
    pub value: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Header {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Debug)]
struct Annotation {
    name: String,
    args: String,
    start: usize,
    end: usize,
}

const MAPPINGS: [&str; 6] = [
    "GetMapping",
    "PostMapping",
    "PutMapping",
    "DeleteMapping",
    "PatchMapping",
    "RequestMapping",
];

pub fn parse_source(source: &str, text: &str) -> Vec<Endpoint> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut from = 0;
    while let Some(controller) =
        find_annotation_named(text, from, &["RestController", "Controller"])
    {
        let Some(class_at) = find_keyword(text, controller.end, "class") else {
            from = controller.end;
            continue;
        };
        let Some((class_name, name_end)) =
            read_ident(text, skip_trivia(text, class_at + "class".len()))
        else {
            from = controller.end;
            continue;
        };
        let Some(body_open) = find_body_open(text, name_end) else {
            from = controller.end;
            continue;
        };
        let Some(body_close) = matching(text, body_open, b'{', b'}') else {
            break;
        };
        let prefix_start = previous_boundary(text, controller.start);
        let class_mapping = last_annotation(&text[prefix_start..class_at], "RequestMapping");
        let package = package_name(text);
        let class_description = doc_before(text, controller.start);
        let (class_paths, class_consumes, class_produces) = match class_mapping {
            Some(annotation) => (
                path_strings(&annotation.args),
                first_string(&annotation.args, "consumes"),
                first_string(&annotation.args, "produces"),
            ),
            None => (Vec::new(), None, None),
        };
        parse_methods(
            source,
            text,
            body_open + 1,
            &text[body_open + 1..body_close],
            &class_name,
            &package,
            &class_description,
            &class_paths,
            class_consumes,
            class_produces,
            &mut out,
            &mut seen,
        );
        from = body_close + 1;
    }
    out
}

#[allow(clippy::too_many_arguments)]
fn parse_methods(
    source: &str,
    text: &str,
    body_offset: usize,
    body: &str,
    controller: &str,
    package: &str,
    class_description: &str,
    class_paths: &[String],
    class_consumes: Option<String>,
    class_produces: Option<String>,
    out: &mut Vec<Endpoint>,
    seen: &mut HashSet<String>,
) {
    let mut from = 0;
    while let Some(annotation) = find_annotation_named(body, from, &MAPPINGS) {
        from = annotation.end;
        let Some((method_name, params, method_name_start)) = method_signature(body, annotation.end)
        else {
            continue;
        };
        if method_name.is_empty() || is_control_keyword(&method_name) {
            continue;
        }
        let mut method_paths = path_strings(&annotation.args);
        if method_paths.is_empty() {
            method_paths.push(String::new());
        }
        let paths = join_paths(class_paths, &method_paths);
        let parsed = parse_params(&params);
        let consumes =
            first_string(&annotation.args, "consumes").or_else(|| class_consumes.clone());
        let produces =
            first_string(&annotation.args, "produces").or_else(|| class_produces.clone());
        let headers = header_strings(&annotation.args);
        let line = line_of(text, body_offset + annotation.start);
        let method_line = line_of(text, body_offset + method_name_start);
        let description = doc_before(body, annotation.start);
        for path in paths {
            let key = format!("{source}:{line}:{method_name}:{path}");
            if !seen.insert(key.clone()) {
                continue;
            }
            out.push(Endpoint {
                key,
                source: source.to_owned(),
                controller: controller.to_owned(),
                controller_package: package.to_owned(),
                controller_description: class_description.to_owned(),
                description: description.clone(),
                method_name: method_name.clone(),
                http_method: mapping_method(&annotation.name, &annotation.args).to_owned(),
                paths: vec![path.clone()],
                path,
                query: parsed.query.clone(),
                headers: headers.clone(),
                consumes: consumes.clone(),
                produces: produces.clone(),
                body_type: parsed.body_type.clone(),
                line,
                method_line,
            });
        }
    }
}

/// A method name, its parenthesized parameter list and the name's offset after `from`.
fn method_signature(body: &str, from: usize) -> Option<(String, String, usize)> {
    let mut at = from;
    loop {
        at = skip_trivia(body, at);
        if body.as_bytes().get(at) == Some(&b'@') {
            if let Some(annotation) = annotation(body, at) {
                at = annotation.end;
                continue;
            }
        }
        break;
    }
    let open = find_call_paren(body, at)?;
    let close = matching(body, open, b'(', b')')?;
    let mut end = open;
    while end > 0 && body.as_bytes()[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_ident(body.as_bytes()[start - 1]) {
        start -= 1;
    }
    if start == end {
        return None;
    }
    Some((
        body[start..end].to_owned(),
        body[open + 1..close].to_owned(),
        start,
    ))
}

fn find_call_paren(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'(' => return Some(i),
            b'{' | b';' | b'=' => return None,
            b'"' | b'\'' | b'`' => i = skip_string(text, i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(bytes.len(), |n| i + n + 1);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(bytes.len(), |n| i + n + 4);
            }
            _ => i += 1,
        }
    }
    None
}

struct Params {
    query: Vec<QueryParam>,
    headers: Vec<Header>,
    body_type: Option<String>,
}

fn parse_params(params: &str) -> Params {
    let mut out = Params {
        query: Vec::new(),
        headers: Vec::new(),
        body_type: None,
    };
    for segment in split_top(params, ',')
        .into_iter()
        .filter(|s| !s.trim().is_empty())
    {
        let annotations = annotations_in(segment);
        let find = |name: &str| annotations.iter().find(|a| a.name == name);
        if find("RequestBody").is_some() {
            out.body_type = param_type(segment);
        } else if let Some(annotation) = find("RequestParam") {
            let name = annotation_string(&annotation.args, &["name", "value"])
                .or_else(|| param_name(segment));
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                let value = annotation_string(&annotation.args, &["defaultValue"]);
                out.query.push(QueryParam { name, value });
            }
        } else if let Some(annotation) = find("RequestHeader") {
            let name = annotation_string(&annotation.args, &["name", "value"])
                .or_else(|| param_name(segment));
            if let Some(name) = name.filter(|n| !n.is_empty()) {
                out.headers.push(Header {
                    name,
                    value: String::new(),
                });
            }
        }
    }
    out
}

fn param_name(segment: &str) -> Option<String> {
    let text = strip_annotations(segment);
    if let Some(colon) = text.find(':') {
        return last_ident(&text[..colon]);
    }
    let before_default = text.split('=').next().unwrap_or(&text);
    before_default
        .split_whitespace()
        .last()
        .and_then(last_ident)
}

fn param_type(segment: &str) -> Option<String> {
    let text = strip_annotations(segment);
    if let Some(colon) = text.find(':') {
        let after = text[colon + 1..].split('=').next().unwrap_or("").trim();
        let after = after.trim_end_matches('?').trim();
        return (!after.is_empty()).then(|| after.to_owned());
    }
    let before_default = text.split('=').next().unwrap_or(&text);
    let mut words: Vec<&str> = before_default
        .split_whitespace()
        .filter(|w| {
            !matches!(
                *w,
                "final" | "val" | "var" | "private" | "public" | "protected"
            )
        })
        .collect();
    if words.len() < 2 {
        return None;
    }
    words.pop();
    Some(words.join(" "))
}

fn strip_annotations(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < text.len() {
        if text.as_bytes()[i] == b'@' {
            if let Some(annotation) = annotation(text, i) {
                out.push(' ');
                i = annotation.end;
                continue;
            }
        }
        let ch = text[i..].chars().next().expect("a char at a char boundary");
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

fn last_ident(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut end = bytes.len();
    while end > 0 && !is_ident(bytes[end - 1]) {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    (start < end).then(|| text[start..end].to_owned())
}

fn annotations_in(text: &str) -> Vec<Annotation> {
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(relative) = text[from..].find('@') {
        let at = from + relative;
        let Some(annotation) = annotation(text, at) else {
            from = at + 1;
            continue;
        };
        from = annotation.end;
        out.push(annotation);
    }
    out
}

fn annotation(text: &str, at: usize) -> Option<Annotation> {
    if text.as_bytes().get(at) != Some(&b'@') {
        return None;
    }
    let bytes = text.as_bytes();
    let mut end = at + 1;
    while end < bytes.len() && is_ident(bytes[end]) {
        end += 1;
    }
    if end == at + 1 {
        return None;
    }
    let name = text[at + 1..end].to_owned();
    let after = skip_trivia(text, end);
    if bytes.get(after) == Some(&b'(') {
        let close = matching(text, after, b'(', b')')?;
        Some(Annotation {
            name,
            args: text[after + 1..close].to_owned(),
            start: at,
            end: close + 1,
        })
    } else {
        Some(Annotation {
            name,
            args: String::new(),
            start: at,
            end,
        })
    }
}

fn find_annotation_named(text: &str, from: usize, names: &[&str]) -> Option<Annotation> {
    let mut at = from;
    while let Some(relative) = text[at..].find('@') {
        let start = at + relative;
        let Some(annotation) = annotation(text, start) else {
            at = start + 1;
            continue;
        };
        if names.contains(&annotation.name.as_str()) {
            return Some(annotation);
        }
        at = annotation.end.max(start + 1);
    }
    None
}

fn last_annotation(text: &str, name: &str) -> Option<Annotation> {
    let mut found = None;
    let mut from = 0;
    while let Some(annotation) = find_annotation_named(text, from, &[name]) {
        from = annotation.end;
        found = Some(annotation);
    }
    found
}

pub(crate) fn matching(text: &str, open: usize, open_byte: u8, close_byte: u8) -> Option<usize> {
    let bytes = text.as_bytes();
    if bytes.get(open) != Some(&open_byte) {
        return None;
    }
    let mut depth = 1u32;
    let mut i = open + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' | b'`' => i = skip_string(text, i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(bytes.len(), |n| i + n + 1);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(bytes.len(), |n| i + n + 4);
            }
            byte if byte == open_byte => {
                depth += 1;
                i += 1;
            }
            byte if byte == close_byte => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
                i += 1;
            }
            _ => i += 1,
        }
    }
    None
}

fn skip_string(text: &str, at: usize) -> usize {
    let bytes = text.as_bytes();
    let quote = bytes[at];
    let mut i = at + 1;
    while i < bytes.len() {
        if bytes[i] == b'\\' {
            i += 2;
            continue;
        }
        if bytes[i] == quote {
            return i + 1;
        }
        i += 1;
    }
    bytes.len()
}

fn skip_trivia(text: &str, from: usize) -> usize {
    let bytes = text.as_bytes();
    let mut i = from;
    loop {
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if bytes.get(i) == Some(&b'/') && bytes.get(i + 1) == Some(&b'/') {
            i = text[i..].find('\n').map_or(bytes.len(), |n| i + n + 1);
            continue;
        }
        if bytes.get(i) == Some(&b'/') && bytes.get(i + 1) == Some(&b'*') {
            i = text[i + 2..].find("*/").map_or(bytes.len(), |n| i + n + 4);
            continue;
        }
        return i;
    }
}

pub(crate) fn find_keyword(text: &str, from: usize, keyword: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut at = from;
    while let Some(relative) = text[at..].find(keyword) {
        let start = at + relative;
        let end = start + keyword.len();
        let before_ok = start == 0 || !is_ident(bytes[start - 1]);
        let after_ok = end >= bytes.len() || !is_ident(bytes[end]);
        if before_ok && after_ok {
            return Some(start);
        }
        at = start + 1;
    }
    None
}

pub(crate) fn read_ident(text: &str, from: usize) -> Option<(String, usize)> {
    let bytes = text.as_bytes();
    let mut end = from;
    while end < bytes.len() && is_ident(bytes[end]) {
        end += 1;
    }
    (end > from).then(|| (text[from..end].to_owned(), end))
}

pub(crate) fn find_body_open(text: &str, from: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut i = from;
    while i < bytes.len() {
        match bytes[i] {
            b'{' => return Some(i),
            b';' => return None,
            b'"' | b'\'' | b'`' => i = skip_string(text, i),
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = text[i..].find('\n').map_or(bytes.len(), |n| i + n + 1);
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = text[i + 2..].find("*/").map_or(bytes.len(), |n| i + n + 4);
            }
            _ => i += 1,
        }
    }
    None
}

fn previous_boundary(text: &str, at: usize) -> usize {
    text[..at].rfind([';', '}']).map_or(0, |i| i + 1)
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

fn is_control_keyword(name: &str) -> bool {
    matches!(
        name,
        "if" | "for" | "while" | "switch" | "catch" | "new" | "return" | "synchronized"
    )
}

fn mapping_method(annotation: &str, args: &str) -> &'static str {
    match annotation {
        "GetMapping" => "GET",
        "PostMapping" => "POST",
        "PutMapping" => "PUT",
        "DeleteMapping" => "DELETE",
        "PatchMapping" => "PATCH",
        _ => {
            let upper = args.to_ascii_uppercase();
            for method in [
                "DELETE", "PATCH", "POST", "PUT", "HEAD", "OPTIONS", "TRACE", "GET",
            ] {
                if upper.contains(&format!("REQUESTMETHOD.{method}")) {
                    return method;
                }
            }
            "GET"
        }
    }
}

fn split_top(text: &str, separator: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut depth = 0i32;
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'{' | b'(' | b'[' => depth += 1,
            b'}' | b')' | b']' => depth -= 1,
            b'"' | b'\'' | b'`' => {
                i = skip_string(text, i);
                continue;
            }
            byte if byte == separator as u8 && depth == 0 => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
        i += 1;
    }
    out.push(&text[start..]);
    out
}

fn strings_in(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if matches!(bytes[i], b'"' | b'\'') {
            let quote = bytes[i] as char;
            let mut value = String::new();
            i += 1;
            while i < bytes.len() {
                let ch = text[i..].chars().next().expect("a char at a char boundary");
                if ch == '\\' {
                    if let Some(next) = text[i + 1..].chars().next() {
                        value.push(next);
                        i += 1 + next.len_utf8();
                        continue;
                    }
                }
                if ch == quote {
                    i += 1;
                    break;
                }
                value.push(ch);
                i += ch.len_utf8();
            }
            out.push(value);
        } else {
            let ch = text[i..].chars().next().expect("a char at a char boundary");
            i += ch.len_utf8();
        }
    }
    out
}

/// Strings that are a mapping's paths (positional, `value =` or `path =`).
fn path_strings(args: &str) -> Vec<String> {
    let mut out = Vec::new();
    for entry in split_top(args, ',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed.split_once('=') {
            Some((key, value)) if matches!(key.trim(), "value" | "path") => {
                out.extend(strings_in(value))
            }
            Some(_) => {}
            None => out.extend(strings_in(trimmed)),
        }
    }
    out
}

fn first_string(args: &str, key: &str) -> Option<String> {
    for entry in split_top(args, ',') {
        let Some((name, value)) = entry.split_once('=') else {
            continue;
        };
        if name.trim() == key {
            return strings_in(value).into_iter().next();
        }
    }
    None
}

fn header_strings(args: &str) -> Vec<Header> {
    let mut out = Vec::new();
    for entry in split_top(args, ',') {
        let Some((name, value)) = entry.split_once('=') else {
            continue;
        };
        if name.trim() != "headers" {
            continue;
        }
        for pair in strings_in(value) {
            if let Some((name, value)) = pair.split_once('=') {
                out.push(Header {
                    name: name.trim().to_owned(),
                    value: value.trim().to_owned(),
                });
            }
        }
    }
    out
}

fn annotation_string(args: &str, keys: &[&str]) -> Option<String> {
    for entry in split_top(args, ',') {
        let trimmed = entry.trim();
        if trimmed.is_empty() {
            continue;
        }
        match trimmed.split_once('=') {
            Some((key, value)) if keys.contains(&key.trim()) => {
                if let Some(value) = strings_in(value).into_iter().next() {
                    return Some(value);
                }
            }
            Some(_) => {}
            None if keys.contains(&"value") => {
                if let Some(value) = strings_in(trimmed).into_iter().next() {
                    return Some(value);
                }
            }
            None => {}
        }
    }
    None
}

fn join_paths(class_paths: &[String], method_paths: &[String]) -> Vec<String> {
    let bases: Vec<&str> = if class_paths.is_empty() {
        vec![""]
    } else {
        class_paths.iter().map(String::as_str).collect()
    };
    let methods: Vec<&str> = if method_paths.is_empty() {
        vec![""]
    } else {
        method_paths.iter().map(String::as_str).collect()
    };
    let mut out = Vec::new();
    for base in &bases {
        for method in &methods {
            let path = join_one(base, method);
            if !out.contains(&path) {
                out.push(path);
            }
        }
    }
    out
}

fn join_one(base: &str, method: &str) -> String {
    let base = base.trim().trim_matches('/');
    let method = method.trim().trim_matches('/');
    match (base.is_empty(), method.is_empty()) {
        (true, true) => "/".to_owned(),
        (true, false) => format!("/{method}"),
        (false, true) => format!("/{base}"),
        (false, false) => format!("/{base}/{method}"),
    }
}

fn line_of(text: &str, at: usize) -> u32 {
    text.as_bytes()[..at.min(text.len())]
        .iter()
        .filter(|b| **b == b'\n')
        .count() as u32
}

fn package_name(text: &str) -> String {
    let Some(at) = find_keyword(text, 0, "package") else {
        return String::new();
    };
    let rest = &text[at + "package".len()..];
    let Some(end) = rest.find(';') else {
        return String::new();
    };
    let name = rest[..end].trim();
    if name
        .split('.')
        .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
    {
        name.to_owned()
    } else {
        String::new()
    }
}

/// The doc comment immediately before `at`, if nothing but annotations/whitespace separates them.
fn doc_before(text: &str, at: usize) -> String {
    let prefix = &text[..at];
    let Some(end) = prefix.rfind("*/") else {
        return String::new();
    };
    let between = &prefix[end + 2..];
    if between.contains('{') || between.contains(';') || between.contains('}') {
        return String::new();
    }
    let Some(open) = prefix[..end].rfind("/**") else {
        return String::new();
    };
    clean_doc(&prefix[open + 3..end])
}

fn clean_doc(comment: &str) -> String {
    let mut lines = Vec::new();
    for line in comment.lines() {
        let line = line.trim().trim_start_matches('*').trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('@') {
            break;
        }
        lines.push(line);
    }
    lines
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    const JAVA: &str = r#"
@RestController
@RequestMapping("/api/users")
public class UserController {
    @GetMapping("/{id}")
    public User get(@PathVariable Long id, @RequestParam(name = "q", defaultValue = "all") String q) {
        return null;
    }

    @PostMapping(path = {"/", "/create"}, consumes = "application/json", produces = "application/json")
    public User create(@RequestBody User body) {
        return null;
    }

    @RequestMapping(value = "/legacy", method = RequestMethod.DELETE, headers = {"X-Key=secret"})
    public void legacy() {}
}
"#;

    const KOTLIN: &str = r#"
@RestController
@RequestMapping("/api/items")
class ItemController {
    @GetMapping("/{id}")
    fun get(@PathVariable id: Long, @RequestParam q: String?): Item = TODO()

    @PostMapping("/create", consumes = "application/json")
    fun create(@RequestBody body: Item): Item = TODO()
}
"#;

    #[test]
    fn reads_java_controllers_and_parameters() {
        let endpoints = parse_source("/p/UserController.java", JAVA);
        assert_eq!(endpoints.len(), 4);

        let get = endpoints.iter().find(|e| e.method_name == "get").unwrap();
        assert_eq!(get.controller, "UserController");
        assert_eq!(get.path, "/api/users/{id}");
        assert_eq!(get.http_method, "GET");
        assert_eq!(
            get.query,
            [QueryParam {
                name: "q".into(),
                value: Some("all".into())
            }],
            "the annotation name and default are read"
        );

        let create = endpoints
            .iter()
            .find(|e| e.method_name == "create")
            .unwrap();
        assert_eq!(create.path, "/api/users");
        assert_eq!(create.http_method, "POST");
        assert_eq!(create.body_type.as_deref(), Some("User"));
        assert_eq!(create.consumes.as_deref(), Some("application/json"));
        assert_eq!(create.produces.as_deref(), Some("application/json"));

        let legacy = endpoints
            .iter()
            .find(|e| e.method_name == "legacy")
            .unwrap();
        assert_eq!(legacy.http_method, "DELETE");
        assert_eq!(legacy.path, "/api/users/legacy");
        assert_eq!(
            legacy.headers,
            [Header {
                name: "X-Key".into(),
                value: "secret".into()
            }]
        );
    }

    #[test]
    fn reads_package_and_doc_comments() {
        let code = r#"package com.qiao.controller;

/**
 * 测试 Controller
 */
@RestController
public class HelloController {
    /**
     * 测试接口
     *
     * @return text
     */
    @GetMapping("/hello")
    public String hello() { return "ok"; }
}
"#;
        let endpoints = parse_source("/p/HelloController.java", code);
        assert_eq!(endpoints.len(), 1);
        let endpoint = &endpoints[0];
        assert_eq!(endpoint.controller_package, "com.qiao.controller");
        assert_eq!(endpoint.controller_description, "测试 Controller");
        assert_eq!(endpoint.description, "测试接口");
    }

    #[test]
    fn reads_kotlin_controllers() {
        let endpoints = parse_source("/p/ItemController.kt", KOTLIN);
        assert_eq!(endpoints.len(), 2);
        let get = endpoints.iter().find(|e| e.method_name == "get").unwrap();
        assert_eq!(get.path, "/api/items/{id}");
        assert_eq!(
            get.query,
            [QueryParam {
                name: "q".into(),
                value: None
            }]
        );
        let create = endpoints
            .iter()
            .find(|e| e.method_name == "create")
            .unwrap();
        assert_eq!(create.body_type.as_deref(), Some("Item"));
    }

    #[test]
    fn joins_paths_and_skips_strings_around_braces() {
        assert_eq!(join_one("/api", "/users"), "/api/users");
        assert_eq!(join_one("/", "/"), "/");
        assert_eq!(
            join_paths(&["/api".into()], &["".into(), "/a".into()]),
            ["/api", "/api/a"]
        );
        let text = "class A { void f() { String s = \"}\"; } }";
        let open = text.find('{').unwrap();
        assert_eq!(matching(text, open, b'{', b'}'), text.rfind('}'));
    }

    #[test]
    fn reads_annotation_arguments() {
        let args = r#"path = {"/a", "/b"}, consumes = "text/plain", headers = {"X-A=1"}"#;
        assert_eq!(path_strings(args), ["/a", "/b"]);
        assert_eq!(
            first_string(args, "consumes").as_deref(),
            Some("text/plain")
        );
        assert_eq!(
            header_strings(args),
            [Header {
                name: "X-A".into(),
                value: "1".into()
            }]
        );
    }
}

/// Best-effort enclosing Java method at a 0-based caret line, for the editor's
/// "Add Method To Trace" action. It handles ordinary class files; a caret on a
/// method signature or in its body is enough.
pub fn method_at_caret(text: &str, line: u32) -> Option<(String, String)> {
    let lines: Vec<&str> = text.lines().collect();
    let target = (line as usize).min(lines.len().saturating_sub(1));
    let package = lines
        .iter()
        .find_map(|line| {
            let line = line.trim();
            line.strip_prefix("package ")
                .map(|rest| rest.trim_end_matches(';').trim())
        })
        .unwrap_or("");
    let class = lines[..=target].iter().rev().find_map(|line| {
        let line = line.trim();
        let marker = "class ";
        let at = line.find(marker)?;
        let rest = &line[at + marker.len()..];
        let name = rest
            .split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .next()
            .unwrap_or("");
        (!name.is_empty()).then_some(name.to_owned())
    })?;
    let control = [
        "if", "for", "while", "switch", "catch", "new", "return", "throw", "assert",
    ];
    let mut method = None;
    for candidate in lines[..=target].iter().rev() {
        let line = candidate.trim();
        if line.is_empty()
            || line.starts_with("//")
            || line.starts_with('*')
            || line.starts_with('@')
        {
            continue;
        }
        if line.contains(';') && !line.contains('{') {
            continue;
        }
        let Some(open) = line.find('(') else { continue };
        let before = line[..open].trim_end();
        let name = before
            .rsplit(|c: char| !(c.is_alphanumeric() || c == '_' || c == '$'))
            .next()
            .unwrap_or("");
        if name.is_empty() || control.contains(&name) || name == "class" || name == "record" {
            continue;
        }
        if before.split_whitespace().count() < 2 && !before.contains('>') {
            continue;
        }
        method = Some(name.to_owned());
        break;
    }
    let method = method?;
    let type_name = if package.is_empty() {
        class
    } else {
        format!("{package}.{class}")
    };
    Some((type_name, method))
}

#[cfg(test)]
mod caret_tests {
    use super::method_at_caret;

    #[test]
    fn finds_the_enclosing_method_and_class() {
        let source = "package com.acme;\nclass Service {\n  public void run() {\n    call();\n  }\n}\n";
        assert_eq!(
            method_at_caret(source, 3),
            Some(("com.acme.Service".into(), "run".into()))
        );
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CallableMethod {
    pub source: String,
    pub class_name: String,
    pub class_name_fq: String,
    pub method_name: String,
    /// 0-based line of the method name.
    pub line: u32,
    /// 0-based line of the method body's closing brace (or the declaration
    /// line for an abstract/interface method).
    pub end_line: u32,
    pub parameter_types: Vec<String>,
    pub parameter_names: Vec<String>,
    pub return_type: Option<String>,
}

/// Every Java method declared directly in a class or interface body, for the
/// "Call this method" gutter action. This is a lexical read, not a compiler.
pub fn parse_callable_methods(source: &str, text: &str) -> Vec<CallableMethod> {
    let package = package_name(text);
    let mut out = Vec::new();
    let mut from = 0;
    loop {
        let class = find_keyword(text, from, "class");
        let interface = find_keyword(text, from, "interface");
        let (keyword_at, keyword) = match (class, interface) {
            (None, None) => break,
            (Some(a), None) => (a, "class"),
            (None, Some(b)) => (b, "interface"),
            (Some(a), Some(b)) if a < b => (a, "class"),
            (Some(_), Some(b)) => (b, "interface"),
        };
        let after = keyword_at + keyword.len();
        let Some((class_name, name_end)) = read_ident(text, skip_trivia(text, after)) else {
            from = after;
            continue;
        };
        let Some(body_open) = find_body_open(text, name_end) else {
            from = after;
            continue;
        };
        let Some(body_close) = matching(text, body_open, b'{', b'}') else {
            break;
        };
        let class_name_fq = if package.is_empty() {
            class_name.clone()
        } else {
            format!("{package}.{class_name}")
        };
        scan_methods(
            source,
            text,
            body_open + 1,
            &text[body_open + 1..body_close],
            &class_name,
            &class_name_fq,
            &mut out,
        );
        from = body_close + 1;
    }
    out
}

fn scan_methods(
    source: &str,
    text: &str,
    offset: usize,
    body: &str,
    class_name: &str,
    class_name_fq: &str,
    out: &mut Vec<CallableMethod>,
) {
    let bytes = body.as_bytes();
    let mut depth = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' | b'\'' => {
                i = skip_string(body, i);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'/') => {
                i = body[i..].find('\n').map_or(bytes.len(), |n| i + n + 1);
                continue;
            }
            b'/' if bytes.get(i + 1) == Some(&b'*') => {
                i = body[i + 2..].find("*/").map_or(bytes.len(), |n| i + n + 4);
                continue;
            }
            b'{' => {
                depth += 1;
                i += 1;
                continue;
            }
            b'}' => {
                depth = depth.saturating_sub(1);
                i += 1;
                continue;
            }
            b'(' if depth == 0 => {
                let Some(close) = matching(body, i, b'(', b')') else {
                    i += 1;
                    continue;
                };
                let Some(name_start) = method_name_start(body, i) else {
                    i = close + 1;
                    continue;
                };
                let name = &body[name_start..i];
                if name == class_name || !is_control_keyword(name) {
                    let after = skip_trivia(body, close + 1);
                    let next = body.as_bytes().get(after).copied();
                    let throws = body[after..].starts_with("throws");
                    if next == Some(b'{') || next == Some(b';') || throws {
                        if is_declaration_prefix(body, name_start) && !out.iter().any(|m| m.line == line_of(text, offset + name_start)) {
                            let params = &body[i + 1..close];
                            let types = split_top(params, ',')
                                .into_iter()
                                .filter(|s| !s.trim().is_empty())
                                .filter_map(|s| param_type(s))
                                .collect::<Vec<_>>();
                            let names = split_top(params, ',')
                                .into_iter()
                                .filter(|s| !s.trim().is_empty())
                                .filter_map(|s| param_name(s))
                                .collect::<Vec<_>>();
                            let prefix_start = previous_boundary(body, name_start);
                            let return_type = body[prefix_start..name_start].split_whitespace().last().map(|s| s.to_owned());
                            let body_end = matching(body, after, b'{', b'}').map(|close| offset + close);
                            out.push(CallableMethod {
                                source: source.to_owned(),
                                class_name: class_name.to_owned(),
                                class_name_fq: class_name_fq.to_owned(),
                                method_name: name.to_owned(),
                                line: line_of(text, offset + name_start),
                                end_line: line_of(text, body_end.unwrap_or(offset + name_start)),
                                parameter_types: types,
                                parameter_names: names,
                                return_type,
                            });
                        }
                    }
                }
                i = close + 1;
                continue;
            }
            _ => {}
        }
        i += 1;
    }
}

fn method_name_start(body: &str, open: usize) -> Option<usize> {
    let bytes = body.as_bytes();
    let mut end = open;
    while end > 0 && bytes[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let mut start = end;
    while start > 0 && is_ident(bytes[start - 1]) {
        start -= 1;
    }
    (start < end).then_some(start)
}

fn is_declaration_prefix(body: &str, name_start: usize) -> bool {
    let bytes = body.as_bytes();
    let mut at = name_start;
    while at > 0 && bytes[at - 1].is_ascii_whitespace() {
        at -= 1;
    }
    if at == 0 {
        return true;
    }
    match bytes[at - 1] {
        b'@' | b'.' | b'=' | b',' | b'(' | b'+' | b'-' | b'*' | b'/' => false,
        b'>' | b']' => true,
        _ => {
            let mut start = at;
            while start > 0 && is_ident(bytes[start - 1]) {
                start -= 1;
            }
            let before = &body[start..at];
            !matches!(before, "return" | "new" | "throw" | "else" | "case" | "yield")
        }
    }
}

#[cfg(test)]
mod callable_tests {
    use super::*;

    #[test]
    fn finds_methods_in_controllers_and_services() {
        let text = r#"
package com.qiao.service;
public interface CategoryService {
    List<Category> queryAllRootLevelCat();
    List<CategoryVO> getSubCatList(Integer rootCatId);
}
class Impl implements CategoryService {
    public List<CategoryVO> getSubCatList(Integer rootCatId) { return null; }
    private static String hidden(String a, int b) { return a; }
}
"#;
        let methods = parse_callable_methods("/p/CategoryService.java", text);
        assert_eq!(methods.len(), 4);
        assert_eq!(methods[1].class_name_fq, "com.qiao.service.CategoryService");
        assert_eq!(methods[1].method_name, "getSubCatList");
        assert_eq!(methods[1].parameter_types, ["Integer"]);
        assert_eq!(methods[1].parameter_names, ["rootCatId"]);
        assert_eq!(methods[1].end_line, methods[1].line);
        assert_eq!(methods[3].class_name, "Impl");
        assert_eq!(methods[3].parameter_types, ["String", "int"]);
    }
}
