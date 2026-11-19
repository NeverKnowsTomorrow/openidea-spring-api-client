//! HTTP Client environments (`http-client.env.json` and its private sibling),
//! read from the nearest module and then the project root.

use openidea_plugin::Project;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvField {
    pub name: String,
    pub value: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Globals {
    pub params: Vec<EnvField>,
    pub headers: Vec<EnvField>,
    pub form: Vec<EnvField>,
}

#[derive(Clone, Debug, Default)]
pub struct Environments {
    pub names: Vec<String>,
    /// Environment name -> flattened shared + environment string values.
    pub values: Map<String, Value>,
    /// Environment name -> global query params, headers and form fields.
    pub globals: BTreeMap<String, Globals>,
}

pub fn load(project: &Project, source: &str) -> Environments {
    let root = project.root().unwrap_or_default();
    let module = crate::request::module_root(source);
    let mut dirs: Vec<String> = Vec::new();
    if let Some(module) = module {
        dirs.push(module);
    }
    if !root.is_empty() && !dirs.contains(&root) {
        dirs.push(root);
    }

    // Nearest first; private before public so it wins.
    let mut files: Vec<Value> = Vec::new();
    for dir in &dirs {
        for name in ["http-client.private.env.json", "http-client.env.json"] {
            let path = format!("{dir}/{name}");
            let Ok(text) = project.read_file(&path) else {
                continue;
            };
            if let Ok(value) = serde_json::from_str::<Value>(&text) {
                if value.is_object() {
                    files.push(value);
                }
            }
        }
    }

    let mut names = Vec::new();
    for file in &files {
        let Some(object) = file.as_object() else {
            continue;
        };
        for key in object.keys().filter(|key| key.as_str() != "$shared") {
            if !names.contains(key) {
                names.push(key.clone());
            }
        }
    }
    names.sort();

    let mut values = Map::new();
    let mut globals = BTreeMap::new();
    for name in names.iter() {
        let mut merged = Map::new();
        let mut global = Globals::default();
        for file in &files {
            let Some(object) = file.as_object() else {
                continue;
            };
            if let Some(shared) = object.get("$shared").and_then(Value::as_object) {
                insert_missing(&mut merged, shared);
                merge_globals(&mut global, shared);
            }
            if let Some(environment) = object.get(name).and_then(Value::as_object) {
                insert_missing(&mut merged, environment);
                merge_globals(&mut global, environment);
            }
        }
        values.insert(name.clone(), Value::Object(merged));
        globals.insert(name.clone(), global);
    }
    Environments {
        names,
        values,
        globals,
    }
}

fn insert_missing(target: &mut Map<String, Value>, source: &Map<String, Value>) {
    for (key, value) in source {
        if target.contains_key(key) {
            continue;
        }
        if let Some(value) = scalar(value) {
            target.insert(key.clone(), value);
        }
    }
}

fn merge_globals(target: &mut Globals, source: &Map<String, Value>) {
    for (key, fields) in [
        ("params", &mut target.params),
        ("headers", &mut target.headers),
        ("form", &mut target.form),
    ] {
        let Some(object) = source.get(key).and_then(Value::as_object) else {
            continue;
        };
        for (name, value) in object {
            let Some(value) = scalar(value) else { continue };
            let name = name.to_owned();
            if fields
                .iter()
                .any(|field| field.name.eq_ignore_ascii_case(&name))
            {
                continue;
            }
            fields.push(EnvField {
                name,
                value: value.as_str().unwrap_or_default().to_owned(),
                enabled: true,
            });
        }
    }
}

fn scalar(value: &Value) -> Option<Value> {
    match value {
        Value::String(text) => Some(Value::String(text.clone())),
        Value::Number(number) => Some(Value::String(number.to_string())),
        Value::Bool(flag) => Some(Value::String(flag.to_string())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use openidea_plugin::mock::MockHost;

    #[test]
    fn merges_shared_and_private_values_with_the_nearest_file_winning() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        host.add_file(
            "/p/http-client.env.json",
            r#"{ "$shared": { "host": "root", "version": "1" }, "dev": { "host": "dev" } }"#,
        );
        host.add_file(
            "/p/module/http-client.env.json",
            r#"{ "$shared": { "host": "module", "headers": { "X-Env": "shared" }, "params": { "debug": "true" } }, "dev": { "token": 42, "headers": { "X-Dev": "1" }, "form": { "app": "foodie" } }, "prod": { "host": "prod" } }"#,
        );
        host.add_file(
            "/p/module/http-client.private.env.json",
            r#"{ "dev": { "secret": "s" } }"#,
        );
        let ctx = host.invocation();
        let project = ctx.project().unwrap();
        let environments = load(&project, "/p/module/src/main/java/x/C.java");
        assert_eq!(environments.names, ["dev", "prod"]);
        let dev = environments.values.get("dev").unwrap();
        assert_eq!(dev["host"], "module");
        assert_eq!(dev["version"], "1");
        assert_eq!(dev["token"], "42");
        assert_eq!(dev["secret"], "s");
        assert_eq!(environments.values["prod"]["host"], "module");
        let globals = environments.globals.get("dev").unwrap();
        assert_eq!(globals.params[0].name, "debug");
        assert_eq!(globals.headers[0].name, "X-Env");
        assert_eq!(globals.headers[1].name, "X-Dev");
        assert_eq!(globals.form[0].value, "foodie");
    }

    #[test]
    fn no_files_is_empty() {
        let host = MockHost::install();
        host.set_project_root(Some("/p"));
        let ctx = host.invocation();
        let project = ctx.project().unwrap();
        assert!(load(&project, "/p/src/C.java").names.is_empty());
    }
}
