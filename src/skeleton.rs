//! A JSON body skeleton for a `@RequestBody` type, read from the DTO's fields.

use openidea_plugin::Project;
use serde_json::{Map, Value};

/// A pretty JSON object for `type_name`, or `{}` when its class is not found.
pub fn body_for(project: &Project, source: &str, type_name: &str) -> String {
    let schema = schema_for(project, source, type_name);
    serde_json::to_string_pretty(&value_for_schema(&schema)).unwrap_or_else(|_| "{}".into())
}

/// Recursive DTO schema used by the JSON editor for nested completion and comments.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BodySchema {
    pub name: String,
    pub r#type: String,
    pub comment: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<BodySchema>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub element: Option<Box<BodySchema>>,
}

pub fn schema_for(project: &Project, source: &str, type_name: &str) -> BodySchema {
    let simple = simple_name(type_name);
    let mut seen = std::collections::HashSet::new();
    schema_for_type(project, source, &simple, &mut seen, 0).unwrap_or_else(|| BodySchema {
        name: String::new(),
        r#type: type_name.to_owned(),
        comment: String::new(),
        kind: "unknown".into(),
        children: Vec::new(),
        element: None,
    })
}

fn schema_for_type(
    project: &Project,
    source: &str,
    type_name: &str,
    seen: &mut std::collections::HashSet<String>,
    depth: usize,
) -> Option<BodySchema> {
    let full = type_name.trim();
    let simple = simple_name(full);
    if simple.is_empty() {
        return None;
    }
    if let Some(element) = array_element(full) {
        let child = schema_for_type(project, source, &element, seen, depth + 1);
        return Some(BodySchema {
            name: String::new(),
            r#type: full.to_owned(),
            comment: String::new(),
            kind: "array".into(),
            children: Vec::new(),
            element: child.map(Box::new),
        });
    }
    if let Some(kind) = primitive_kind(&simple) {
        return Some(BodySchema {
            name: String::new(),
            r#type: simple,
            comment: String::new(),
            kind: kind.into(),
            children: Vec::new(),
            element: None,
        });
    }
    if depth >= 4 || !seen.insert(simple.clone()) {
        return Some(BodySchema {
            name: String::new(),
            r#type: simple,
            comment: String::new(),
            kind: "object".into(),
            children: Vec::new(),
            element: None,
        });
    }
    let (text, _path) = class_source(project, source, &simple)?;
    let fields = fields_from_source(&text, &simple);
    let comments = field_comments(&text, &fields);
    let children = fields
        .iter()
        .filter_map(|(name, field_type)| {
            let mut child = schema_for_type(project, source, field_type, seen, depth + 1)?;
            child.name = name.clone();
            child.comment = comments.get(name).cloned().unwrap_or_default();
            Some(child)
        })
        .collect();
    seen.remove(&simple);
    Some(BodySchema {
        name: String::new(),
        r#type: simple,
        comment: String::new(),
        kind: "object".into(),
        children,
        element: None,
    })
}

fn class_source(project: &Project, source: &str, simple: &str) -> Option<(String, String)> {
    let mut candidates = Vec::new();
    for extension in ["java", "kt"] {
        if let Ok(found) = project.find_files(&format!("**/{simple}.{extension}"), 20) {
            candidates.extend(found);
        }
    }
    candidates.sort_by_key(|path| std::cmp::Reverse(common_prefix(path, source)));
    for path in candidates {
        if let Ok(text) = project.read_file(&path) {
            return Some((text, path));
        }
    }
    None
}

fn array_element(type_name: &str) -> Option<String> {
    let type_name = type_name.trim();
    if let Some(inner) = type_name.strip_suffix("[]") {
        return Some(inner.trim().to_owned());
    }
    let open = type_name.find('<')?;
    let name = type_name[..open].trim().rsplit('.').next()?;
    if !matches!(
        name,
        "List" | "Set" | "Collection" | "Iterable" | "Array" | "Flux" | "Optional"
    ) {
        return None;
    }
    let close = type_name.rfind('>')?;
    let inner = type_name[open + 1..close].trim();
    let inner = inner.split(',').next().unwrap_or(inner).trim();
    (!inner.is_empty()).then(|| inner.to_owned())
}

fn primitive_kind(type_name: &str) -> Option<&'static str> {
    let base = type_name
        .trim_end_matches('?')
        .rsplit('.')
        .next()
        .unwrap_or(type_name);
    let base = base.split('<').next().unwrap_or(base).trim();
    if matches!(
        base,
        "String"
            | "CharSequence"
            | "char"
            | "Character"
            | "UUID"
            | "LocalDate"
            | "LocalDateTime"
            | "Instant"
            | "Date"
    ) {
        return Some("string");
    }
    if matches!(
        base,
        "int"
            | "Integer"
            | "long"
            | "Long"
            | "short"
            | "Short"
            | "byte"
            | "Byte"
            | "float"
            | "Float"
            | "double"
            | "Double"
            | "BigDecimal"
            | "BigInteger"
            | "Number"
            | "Int"
    ) {
        return Some("number");
    }
    if matches!(base, "boolean" | "Boolean") {
        return Some("boolean");
    }
    if matches!(
        base,
        "Map" | "HashMap" | "LinkedHashMap" | "JsonNode" | "ObjectNode" | "Object"
    ) {
        return Some("object");
    }
    None
}

fn value_for_schema(schema: &BodySchema) -> Value {
    match schema.kind.as_str() {
        "object" => {
            let mut map = Map::new();
            for child in &schema.children {
                map.insert(child.name.clone(), value_for_schema(child));
            }
            Value::Object(map)
        }
        "array" => Value::Array(Vec::new()),
        "string" => Value::String(String::new()),
        "number" => Value::Number(0.into()),
        "boolean" => Value::Bool(false),
        _ => Value::Object(Map::new()),
    }
}

fn simple_name(type_name: &str) -> String {
    let type_name = type_name.trim().trim_end_matches('?');
    let type_name = type_name.split('<').next().unwrap_or(type_name);
    type_name
        .rsplit('.')
        .next()
        .unwrap_or(type_name)
        .trim()
        .to_owned()
}

fn primitive_value(type_name: &str) -> Option<Value> {
    let base = type_name.trim_end_matches("[]").trim_end_matches('?');
    let base = base.rsplit('.').next().unwrap_or(base);
    let base = base.split('<').next().unwrap_or(base).trim();
    if base.ends_with("[]")
        || matches!(
            base,
            "List" | "Set" | "Collection" | "Iterable" | "Array" | "Flux"
        )
    {
        return Some(Value::Array(Vec::new()));
    }
    if matches!(
        base,
        "Map" | "HashMap" | "LinkedHashMap" | "JsonNode" | "ObjectNode" | "Object"
    ) {
        return Some(Value::Object(Map::new()));
    }
    if matches!(
        base,
        "String"
            | "CharSequence"
            | "char"
            | "Character"
            | "UUID"
            | "LocalDate"
            | "LocalDateTime"
            | "Instant"
            | "Date"
    ) {
        return Some(Value::String(String::new()));
    }
    if matches!(
        base,
        "int"
            | "Integer"
            | "long"
            | "Long"
            | "short"
            | "Short"
            | "byte"
            | "Byte"
            | "float"
            | "Float"
            | "double"
            | "Double"
            | "BigDecimal"
            | "BigInteger"
            | "Number"
            | "Int"
    ) {
        return Some(Value::Number(0.into()));
    }
    if matches!(base, "boolean" | "Boolean") {
        return Some(Value::Bool(false));
    }
    None
}

fn value_for(field_type: &str) -> Value {
    primitive_value(field_type).unwrap_or_else(|| Value::Object(Map::new()))
}

fn common_prefix(a: &str, b: &str) -> usize {
    a.bytes().zip(b.bytes()).take_while(|(a, b)| a == b).count()
}

/// `(name, type)` pairs of the class `simple` in `text` (Java fields and Kotlin
/// properties, including a Kotlin primary constructor).
pub fn fields_from_source(text: &str, simple: &str) -> Vec<(String, String)> {
    let needle = format!("class {simple}");
    let Some(at) = text.find(&needle) else {
        return Vec::new();
    };
    let after = at + needle.len();
    let Some(open) = crate::parser::find_body_open(text, after) else {
        return Vec::new();
    };
    let Some(close) = crate::parser::matching(text, open, b'{', b'}') else {
        return Vec::new();
    };
    let declaration = &text[after..open];
    let body = &text[open + 1..close];

    let mut fields = Vec::new();
    if declaration.contains('(') {
        fields.extend(kotlin_fields(declaration));
    }
    let kotlin =
        body.contains("fun ") || declaration.contains("val ") || declaration.contains("var ");
    if kotlin {
        fields.extend(kotlin_fields(body));
    } else {
        fields.extend(java_fields(body));
    }
    let mut seen = std::collections::HashSet::new();
    fields.retain(|(name, _)| seen.insert(name.clone()));
    fields
}

fn java_fields(body: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for line in body.lines() {
        let line = line.split("//").next().unwrap_or(line).trim();
        if line.is_empty() || line.starts_with('@') || line.contains('(') || line.contains(')') {
            continue;
        }
        for statement in line.split(';') {
            let statement = statement.split('=').next().unwrap_or(statement).trim();
            if statement.is_empty()
                || statement.contains(" class ")
                || statement.starts_with("class ")
            {
                continue;
            }
            if statement.starts_with('*') || statement.starts_with("/*") {
                continue;
            }
            let tokens: Vec<&str> = statement
                .split_whitespace()
                .filter(|token| {
                    !token.starts_with('@')
                        && !matches!(
                            *token,
                            "private" | "public" | "protected" | "final" | "transient" | "volatile"
                        )
                })
                .collect();
            if tokens.len() < 2 || tokens[0] == "static" || statement.contains("static ") {
                continue;
            }
            let name = tokens[tokens.len() - 1].trim();
            if !name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '$')
                || name.is_empty()
            {
                continue;
            }
            let field_type = tokens[..tokens.len() - 1].join(" ");
            if !field_type.is_empty() {
                out.push((name.to_owned(), field_type));
            }
        }
    }
    out
}

fn kotlin_fields(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut from = 0;
    while from < text.len() {
        let value_at = find_word(&text[from..], "val").map(|index| from + index);
        let variable_at = find_word(&text[from..], "var").map(|index| from + index);
        let Some(at) = (match (value_at, variable_at) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }) else {
            break;
        };
        let mut cursor = at + 3;
        while text
            .as_bytes()
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            cursor += 1;
        }
        let Some((name, length)) = read_word(&text[cursor..]) else {
            from = at + 3;
            continue;
        };
        cursor += length;
        while text
            .as_bytes()
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            cursor += 1;
        }
        if text.as_bytes().get(cursor) != Some(&b':') {
            from = at + 3;
            continue;
        }
        cursor += 1;
        while text
            .as_bytes()
            .get(cursor)
            .is_some_and(|byte| byte.is_ascii_whitespace())
        {
            cursor += 1;
        }
        let end = text[cursor..]
            .find([',', ')', '=', '\n'])
            .map_or(text.len(), |index| cursor + index);
        let field_type = text[cursor..end].trim().trim_end_matches('?').trim();
        if !field_type.is_empty() {
            out.push((name, field_type.to_owned()));
        }
        from = end.max(at + 3);
    }
    out
}

fn find_word(text: &str, word: &str) -> Option<usize> {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(relative) = text[from..].find(word) {
        let at = from + relative;
        let before = at == 0 || !is_ident(bytes[at - 1]);
        let after = at + word.len() >= bytes.len() || !is_ident(bytes[at + word.len()]);
        if before && after {
            return Some(at);
        }
        from = at + 1;
    }
    None
}

fn read_word(text: &str) -> Option<(String, usize)> {
    let end = text
        .bytes()
        .position(|byte| !is_ident(byte))
        .unwrap_or(text.len());
    (end > 0).then(|| (text[..end].to_owned(), end))
}

fn is_ident(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_java_fields_and_values() {
        let text = r#"
public class User {
    private String name;
    private Integer age;
    private Boolean active;
    private java.util.List<String> tags;
    private Address address;
    private static final long serialVersionUID = 1L;
}
"#;
        let fields = fields_from_source(text, "User");
        assert!(fields.contains(&("name".into(), "String".into())));
        assert!(fields.contains(&("age".into(), "Integer".into())));
        assert!(fields.contains(&("active".into(), "Boolean".into())));
        assert!(fields.contains(&("tags".into(), "java.util.List<String>".into())));
        assert!(fields.contains(&("address".into(), "Address".into())));
        assert!(!fields.iter().any(|(name, _)| name == "serialVersionUID"));
        assert_eq!(value_for("String"), Value::String(String::new()));
        assert_eq!(value_for("Boolean"), Value::Bool(false));
        assert_eq!(value_for("List<String>"), Value::Array(Vec::new()));
    }

    #[test]
    fn reads_kotlin_data_class_fields() {
        let text = r#"
data class Item(
    val name: String,
    var count: Int?,
    val tags: List<String> = emptyList()
) {
    val internal: Boolean = false
}
"#;
        let fields = fields_from_source(text, "Item");
        assert!(fields.contains(&("name".into(), "String".into())));
        assert!(fields.contains(&("count".into(), "Int".into())));
        assert!(fields.contains(&("tags".into(), "List<String>".into())));
        assert!(fields.contains(&("internal".into(), "Boolean".into())));
    }

    #[test]
    fn builds_a_body_from_a_dto_file() {
        let host = openidea_plugin::mock::MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(
            "/p/module/src/main/java/com/qiao/User.java",
            "public class User { private String name; private Integer age; }",
        );
        let ctx = host.invocation();
        let project = ctx.project().unwrap();
        let body = body_for(
            &project,
            "/p/module/src/main/java/com/qiao/Controller.java",
            "com.qiao.User",
        );
        assert!(body.contains("\"name\": \"\""));
        assert!(body.contains("\"age\": 0"));
    }

    #[test]
    fn builds_a_nested_schema() {
        let host = openidea_plugin::mock::MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(
            "/p/module/src/main/java/com/qiao/Order.java",
            "public class Order {\n/** 收货地址 */\nprivate Address address;\n/** 明细 */\nprivate List<Item> items;\n}",
        );
        host.add_file(
            "/p/module/src/main/java/com/qiao/Address.java",
            "public class Address {\n/** 城市 */\nprivate String city;\n/** 街道 */\nprivate String street;\n}",
        );
        host.add_file(
            "/p/module/src/main/java/com/qiao/Item.java",
            "public class Item {\n/** 数量 */\nprivate Integer count;\n}",
        );
        let ctx = host.invocation();
        let project = ctx.project().unwrap();
        let source = "/p/module/src/main/java/com/qiao/Controller.java";
        let schema = schema_for(&project, source, "com.qiao.Order");
        assert_eq!(schema.kind, "object");
        assert_eq!(schema.children[0].name, "address");
        assert_eq!(schema.children[0].comment, "收货地址");
        assert_eq!(schema.children[0].children[0].comment, "城市");
        assert_eq!(schema.children[1].kind, "array");
        assert_eq!(
            schema.children[1].element.as_ref().unwrap().children[0].comment,
            "数量"
        );
        let body = body_for(&project, source, "com.qiao.Order");
        assert!(body.contains("\"city\": \"\""));
    }

    #[test]
    fn carries_field_comments_into_the_body() {
        let host = openidea_plugin::mock::MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(
            "/p/module/src/main/java/com/qiao/User.java",
            "public class User {\n/** 用户 ID */\nprivate String name;\n/** 年龄 */\nprivate Integer age;\n}",
        );
        let ctx = host.invocation();
        let project = ctx.project().unwrap();
        let source = "/p/module/src/main/java/com/qiao/Controller.java";
        let body = body_for(&project, source, "com.qiao.User");
        assert!(body.contains("\"name\": \"\""));
        assert!(body.contains("\"age\": 0"));
        assert!(!body.contains("//"), "{body}");
        let fields = fields_with_comments(&project, source, "com.qiao.User");
        assert_eq!(fields[0].comment, "用户 ID");
        assert_eq!(fields[1].comment, "年龄");
    }
}

/// A body field plus the Javadoc/line comment above it, when the source has one.
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BodyField {
    pub name: String,
    pub r#type: String,
    pub comment: String,
}

/// Body fields for a `@RequestBody` DTO, with comments read from its source.
pub fn fields_with_comments(project: &Project, source: &str, type_name: &str) -> Vec<BodyField> {
    let simple = simple_name(type_name);
    if simple.is_empty() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for extension in ["java", "kt"] {
        if let Ok(found) = project.find_files(&format!("**/{simple}.{extension}"), 20) {
            candidates.extend(found);
        }
    }
    candidates.sort_by_key(|path| std::cmp::Reverse(common_prefix(path, source)));
    for path in candidates {
        let Ok(text) = project.read_file(&path) else {
            continue;
        };
        let fields = fields_from_source(&text, &simple);
        if fields.is_empty() {
            continue;
        }
        let comments = field_comments(&text, &fields);
        return fields
            .into_iter()
            .map(|(name, field_type)| BodyField {
                comment: comments.get(&name).cloned().unwrap_or_default(),
                name,
                r#type: field_type,
            })
            .collect();
    }
    Vec::new()
}

fn field_comments(
    text: &str,
    fields: &[(String, String)],
) -> std::collections::HashMap<String, String> {
    let names: std::collections::HashSet<&str> =
        fields.iter().map(|(name, _)| name.as_str()).collect();
    let mut out = std::collections::HashMap::new();
    let mut pending = String::new();
    let mut in_doc = false;
    for raw in text.lines() {
        let line = raw.trim();
        if in_doc {
            if let Some(end) = line.find("*/") {
                let part = line[..end].trim().trim_start_matches('*').trim();
                if !part.is_empty() {
                    pending.push(' ');
                    pending.push_str(part);
                }
                in_doc = false;
            } else {
                let part = line.trim_start_matches('*').trim();
                if !part.is_empty() {
                    pending.push(' ');
                    pending.push_str(part);
                }
            }
            continue;
        }
        if line.starts_with("/**") {
            in_doc = !line.contains("*/");
            pending.clear();
            let part = line.trim_start_matches("/**").trim_end_matches("*/").trim();
            if !part.is_empty() {
                pending.push_str(part);
            }
            continue;
        }
        if line.is_empty() || line.starts_with('@') || line.starts_with("//") {
            continue;
        }
        let inline = line
            .find("//")
            .map(|at| line[at + 2..].trim())
            .unwrap_or_default();
        let declaration = line.split("//").next().unwrap_or(line);
        for name in &names {
            if has_word(declaration, name)
                && (declaration.ends_with(';')
                    || declaration.contains('=')
                    || declaration.ends_with(','))
            {
                let mut comment = pending.trim().to_owned();
                if comment.is_empty() && !inline.is_empty() {
                    comment = inline.to_owned();
                }
                if !comment.is_empty() {
                    out.insert((*name).to_owned(), comment);
                }
                pending.clear();
                break;
            }
        }
        if !line.ends_with(',') {
            pending.clear();
        }
    }
    out
}

fn has_word(text: &str, word: &str) -> bool {
    let bytes = text.as_bytes();
    let mut from = 0;
    while let Some(relative) = text[from..].find(word) {
        let at = from + relative;
        let before = at == 0 || !is_ident(bytes[at - 1]);
        let after = at + word.len() >= bytes.len() || !is_ident(bytes[at + word.len()]);
        if before && after {
            return true;
        }
        from = at + 1;
    }
    false
}
