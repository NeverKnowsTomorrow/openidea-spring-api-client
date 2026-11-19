//! Spring API Client: a tool window that lists Spring mappings, builds a request,
//! sends it through the plugin host, and can copy it as cURL or save it as `.http`.
//!
//! All project access goes through the plugin API (`project.read` / `project.write`),
//! all HTTP through `host::fetch`, and the page is an HTML body that talks to this
//! instance with view messages.

mod env;
mod gateway;
mod openapi;
mod parser;
mod request;
mod skeleton;

use std::time::{Instant, SystemTime, UNIX_EPOCH};

use openidea_plugin::language_features::{Document, GutterAction, HoverAction, HoverActions, LanguageFeatures};
use openidea_plugin::views::{Body, View, ViewEvent, Views};
use openidea_plugin::{Error, Invocation, PickItem, Plugin, TextQuery, host, register_plugin};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use env::{EnvField, Globals};
use parser::Endpoint;
use request::Spec;

pub const VIEW: &str = "api-client";
const PAGE: &str = include_str!("page.html");
const BODY_EDITOR_JS: &str = include_str!("body-editor.js");
const MAX_SOURCES: usize = 300;
const MAX_BODY: usize = 1 << 20;
const HISTORY_KEY: &str = "api-client.history";
const HISTORY_LIMIT: usize = 30;
const CACHE_KEY: &str = "api-client.cache";
const CACHE_LIMIT: usize = 50;
const CACHE_BODY: usize = 256 * 1024;
const COLLECTIONS_KEY: &str = "api-client.collections";
const COLLECTIONS_LIMIT: usize = 50;
const COOKIES_KEY: &str = "api-client.cookies";
const ENVS_KEY: &str = "api-client.envs";
const TABS_KEY: &str = "api-client.tabs";
const TABS_LIMIT: usize = 20;
const STRESS_LIMIT: u32 = 200;

#[derive(Clone, Serialize, Deserialize)]
struct CachedRequest {
    draft: Value,
    response: Option<Value>,
    at: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct SavedRequest {
    id: String,
    name: String,
    #[serde(default)]
    folder: String,
    draft: Value,
    request: Value,
    key: Option<String>,
    at: u64,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredEnv {
    #[serde(default)]
    host: String,
    #[serde(default)]
    params: Vec<EnvField>,
    #[serde(default)]
    headers: Vec<EnvField>,
    #[serde(default)]
    form: Vec<EnvField>,
}

#[derive(Default)]
struct ApiClient {
    endpoints: Vec<Endpoint>,
    /// Endpoint selected from the IDE's generated-request action, waiting for the page.
    pending: Option<String>,
    socket: Option<host::WebSocket>,
    trace_targets: Vec<Value>,
}

impl ApiClient {
    fn post(&self, value: Value) -> bool {
        host::post_view_message(VIEW, &value.to_string()).is_ok()
    }

    fn history(&self) -> Vec<Value> {
        host::storage::get(HISTORY_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str::<Vec<Value>>(text.as_str()).ok())
            .unwrap_or_default()
    }

    fn record_history(&self, entry: Value) -> Vec<Value> {
        let mut entries = self.history();
        entries.insert(0, entry);
        entries.truncate(HISTORY_LIMIT);
        if let Ok(text) = serde_json::to_string(&entries) {
            let _ = host::storage::set(HISTORY_KEY, &text);
        }
        entries
    }

    fn post_history(&self, entries: &[Value]) {
        self.post(json!({ "op": "history", "entries": entries }));
    }

    fn cache(&self) -> std::collections::BTreeMap<String, CachedRequest> {
        host::storage::get(CACHE_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(text.as_str()).ok())
            .unwrap_or_default()
    }

    fn save_cache(&self, key: &str, draft: Option<Value>, response: Option<Value>) {
        let Some(draft) = draft else { return };
        let mut cache = self.cache();
        cache.insert(
            key.to_owned(),
            CachedRequest {
                draft,
                response: response.map(|response| trim_cached_response(response, CACHE_BODY)),
                at: now_ms(),
            },
        );
        while cache.len() > CACHE_LIMIT {
            let Some(oldest) = cache
                .iter()
                .min_by_key(|(_, entry)| entry.at)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            cache.remove(&oldest);
        }
        if let Ok(text) = serde_json::to_string(&cache) {
            let _ = host::storage::set(CACHE_KEY, &text);
        }
    }

    fn collections(&self) -> Vec<SavedRequest> {
        host::storage::get(COLLECTIONS_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(text.as_str()).ok())
            .unwrap_or_default()
    }

    fn post_collections(&self, entries: &[SavedRequest]) {
        self.post(json!({ "op": "collections", "entries": entries }));
    }

    fn save_collection(
        &self,
        name: &str,
        folder: &str,
        draft: Value,
        request: Value,
        key: Option<String>,
    ) -> Vec<SavedRequest> {
        let mut entries = self.collections();
        let at = now_ms();
        entries.insert(
            0,
            SavedRequest {
                id: format!("{at}-{}", entries.len()),
                name: name.to_owned(),
                folder: folder.trim().to_owned(),
                draft,
                request,
                key,
                at,
            },
        );
        entries.truncate(COLLECTIONS_LIMIT);
        if let Ok(text) = serde_json::to_string(&entries) {
            let _ = host::storage::set(COLLECTIONS_KEY, &text);
        }
        entries
    }

    fn delete_collection(&self, id: &str) -> Vec<SavedRequest> {
        let mut entries = self.collections();
        entries.retain(|entry| entry.id != id);
        if let Ok(text) = serde_json::to_string(&entries) {
            let _ = host::storage::set(COLLECTIONS_KEY, &text);
        }
        entries
    }

    fn rename_collection(&self, id: &str, name: &str) -> Vec<SavedRequest> {
        let mut entries = self.collections();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
            entry.name = name.trim().to_owned();
        }
        if let Ok(text) = serde_json::to_string(&entries) {
            let _ = host::storage::set(COLLECTIONS_KEY, &text);
        }
        entries
    }

    fn move_collection(&self, id: &str, folder: &str) -> Vec<SavedRequest> {
        let mut entries = self.collections();
        if let Some(entry) = entries.iter_mut().find(|entry| entry.id == id) {
            entry.folder = folder.trim().to_owned();
        }
        if let Ok(text) = serde_json::to_string(&entries) {
            let _ = host::storage::set(COLLECTIONS_KEY, &text);
        }
        entries
    }

    fn run_collections(&self, ctx: &Invocation, ids: &[String], iterations: u32) -> Vec<Value> {
        let iterations = iterations.clamp(1, 20);
        let entries: Vec<SavedRequest> = self
            .collections()
            .into_iter()
            .filter(|entry| ids.is_empty() || ids.contains(&entry.id))
            .collect();
        let project = ctx.project();
        let task = host::begin_task("Run API collection", true);
        let total = entries.len() as u32 * iterations;
        let mut results = Vec::new();
        let mut ok = 0usize;
        let mut failed = 0usize;
        let mut done = 0u32;
        'runs: for iteration in 1..=iterations {
            for entry in &entries {
                if task.is_cancelled() {
                    break 'runs;
                }
                let started = Instant::now();
                let result = serde_json::from_value::<Spec>(entry.request.clone())
                    .map_err(|error| Error::Invalid(error.to_string()))
                    .map(|spec| self.with_cookies(spec))
                    .and_then(|spec| request::to_http(&spec, project.as_ref()))
                    .and_then(|request| host::fetch(&request));
                let elapsed = started.elapsed().as_millis() as u64;
                let (status, error) = match result {
                    Ok(response) => (Some(response.status), None),
                    Err(error) => (None, Some(error.to_string())),
                };
                if error.is_none() {
                    ok += 1;
                } else {
                    failed += 1;
                }
                let result = json!({
                    "id": entry.id,
                    "name": entry.name,
                    "iteration": iteration,
                    "iterations": iterations,
                    "status": status,
                    "error": error,
                    "ms": elapsed,
                });
                self.post(json!({ "op": "runProgress", "result": result }));
                results.push(result);
                done += 1;
                task.report(
                    done,
                    Some(total),
                    Some(format!("{done} / {total}").as_str()),
                );
            }
        }
        drop(task);
        let summary = json!({
            "op": "runDone",
            "ok": ok,
            "failed": failed,
            "total": done,
            "iterations": iterations,
            "results": results,
        });
        self.post(summary.clone());
        summary["results"].as_array().cloned().unwrap_or_default()
    }

    fn stress(&self, ctx: &Invocation, spec: Spec, count: u32) {
        let count = count.clamp(1, STRESS_LIMIT);
        let project = ctx.project();
        let task = host::begin_task("Stress test", true);
        let mut ok = 0u32;
        let mut failed = 0u32;
        let mut total_ms = 0u64;
        let mut min_ms = u64::MAX;
        let mut max_ms = 0u64;
        for index in 0..count {
            if task.is_cancelled() {
                break;
            }
            let started = Instant::now();
            let result =
                request::to_http(&spec, project.as_ref()).and_then(|request| host::fetch(&request));
            let elapsed = started.elapsed().as_millis() as u64;
            match result {
                Ok(_) => ok += 1,
                Err(_) => failed += 1,
            }
            total_ms += elapsed;
            min_ms = min_ms.min(elapsed);
            max_ms = max_ms.max(elapsed);
            if index % 5 == 0 || index + 1 == count {
                self.post(json!({
                    "op": "stressProgress",
                    "done": index + 1,
                    "total": count,
                    "ok": ok,
                    "failed": failed,
                    "avgMs": total_ms / u64::from(index + 1),
                }));
            }
            task.report(
                index + 1,
                Some(count),
                Some(format!("{} / {count}", index + 1).as_str()),
            );
        }
        drop(task);
        self.post(json!({
            "op": "stressDone",
            "done": ok + failed,
            "total": count,
            "ok": ok,
            "failed": failed,
            "avgMs": total_ms / u64::from(ok + failed.max(1)),
            "minMs": if min_ms == u64::MAX { 0 } else { min_ms },
            "maxMs": max_ms,
        }));
    }

    fn cookies(
        &self,
    ) -> std::collections::BTreeMap<String, std::collections::BTreeMap<String, String>> {
        host::storage::get(COOKIES_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(text.as_str()).ok())
            .unwrap_or_default()
    }

    fn with_cookies(&self, mut spec: Spec) -> Spec {
        if spec
            .headers
            .iter()
            .any(|header| header.name.eq_ignore_ascii_case("cookie"))
        {
            return spec;
        }
        let Some(host) = url_host(&spec.url) else {
            return spec;
        };
        let all = self.cookies();
        let Some(jar) = all.get(&host) else {
            return spec;
        };
        let value = jar
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ");
        if !value.is_empty() {
            spec.headers.push(request::Header {
                name: "Cookie".into(),
                value,
            });
        }
        spec
    }

    fn store_cookies(&self, url: &str, headers: &[(String, String)]) {
        let Some(host) = url_host(url) else { return };
        let mut all = self.cookies();
        let jar = all.entry(host).or_default();
        let mut changed = false;
        for (name, value) in headers {
            if !name.eq_ignore_ascii_case("set-cookie") {
                continue;
            }
            let first = value.split(';').next().unwrap_or_default();
            let Some((cookie_name, cookie_value)) = first.split_once('=') else {
                continue;
            };
            let cookie_name = cookie_name.trim();
            if cookie_name.is_empty() {
                continue;
            }
            if cookie_value.is_empty() || value.to_ascii_lowercase().contains("max-age=0") {
                changed |= jar.remove(cookie_name).is_some();
            } else {
                jar.insert(cookie_name.to_owned(), cookie_value.trim().to_owned());
                changed = true;
            }
        }
        if changed && let Ok(text) = serde_json::to_string(&all) {
            let _ = host::storage::set(COOKIES_KEY, &text);
        }
    }

    fn stored_envs(&self) -> std::collections::BTreeMap<String, StoredEnv> {
        host::storage::get(ENVS_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str(text.as_str()).ok())
            .unwrap_or_default()
    }

    fn post_tabs(&self) {
        let stored = host::storage::get(TABS_KEY)
            .ok()
            .flatten()
            .and_then(|text| serde_json::from_str::<Value>(text.as_str()).ok())
            .unwrap_or_else(|| json!({ "tabs": [], "active": null }));
        self.post(json!({
            "op": "tabs",
            "tabs": stored["tabs"].clone(),
            "active": stored["active"].clone(),
        }));
    }

    fn post_environments(&self, ctx: &Invocation) {
        let mut environments = ctx
            .project()
            .map(|project| {
                let source = self
                    .endpoints
                    .first()
                    .map(|endpoint| endpoint.source.as_str())
                    .unwrap_or_default();
                env::load(&project, source)
            })
            .unwrap_or_default();
        if let Some(project) = ctx.project() {
            for route in gateway::discover(&project) {
                if !environments.names.contains(&route.id) {
                    environments.names.push(route.id.clone());
                }
                let values = environments
                    .values
                    .entry(route.id.clone())
                    .or_insert_with(|| Value::Object(Default::default()));
                if let Some(object) = values.as_object_mut() {
                    object.insert("host".into(), Value::String(route.uri));
                    if let Some(path) = route.path {
                        object.insert("gatewayPath".into(), Value::String(path));
                    }
                }
            }
        }
        for (name, stored) in self.stored_envs() {
            if !environments.names.contains(&name) {
                environments.names.push(name.clone());
            }
            if !stored.host.is_empty() {
                let values = environments
                    .values
                    .entry(name.clone())
                    .or_insert_with(|| Value::Object(Default::default()));
                if let Some(object) = values.as_object_mut() {
                    object.insert("host".into(), Value::String(stored.host));
                }
            }
            environments.globals.insert(
                name,
                Globals {
                    params: stored.params,
                    headers: stored.headers,
                    form: stored.form,
                },
            );
        }
        environments.names.sort();
        self.post(json!({
            "op": "environments",
            "names": environments.names,
            "values": environments.values,
            "globals": environments.globals,
        }));
    }

    fn fail(&self, error: &Error) {
        let hint = match error {
            Error::Denied(capability) if capability.starts_with("net.") => {
                "The host needs a network permission. HTTPS and localhost may be approved on first use; plain HTTP to another host must be listed as net.http:<host> in the plugin's permissions."
            }
            Error::Denied(_capability) => {
                "The plugin is missing a permission: open Settings › Plugins and grant it, then try again."
            }
            _ => "",
        };
        self.post(json!({ "op": "error", "message": error.to_string(), "hint": hint }));
    }

    fn scan(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let Some(project) = ctx.project() else {
            self.post(
                json!({ "op": "endpoints", "error": "Open a project first.", "controllers": [] }),
            );
            return Ok(());
        };
        let root = project.root()?;
        let mut sources = Vec::new();
        let query = TextQuery {
            pattern: "@(RestController|Controller)".into(),
            regex: true,
            case_sensitive: true,
            ..TextQuery::default()
        };
        if let Ok(found) = project.search_text(&query, 5000) {
            sources.extend(found.matches.into_iter().map(|m| m.location.path));
        }
        if sources.is_empty() {
            for glob in ["**/*.java", "**/*.kt"] {
                if let Ok(found) = project.find_files(glob, MAX_SOURCES as u32) {
                    sources.extend(found);
                }
            }
        }
        sources.retain(|path| path.ends_with(".java") || path.ends_with(".kt"));
        sources.sort();
        sources.dedup();
        sources.truncate(MAX_SOURCES);

        let mut endpoints = Vec::new();
        for source in sources {
            if let Ok(text) = project.read_file(&source) {
                endpoints.extend(parser::parse_source(&source, &text));
            }
        }
        endpoints.sort_by(|a, b| {
            a.controller
                .cmp(&b.controller)
                .then(a.path.cmp(&b.path))
                .then(a.http_method.cmp(&b.http_method))
        });
        self.endpoints = endpoints;

        let mut groups: std::collections::BTreeMap<String, Vec<Value>> =
            std::collections::BTreeMap::new();
        for endpoint in &self.endpoints {
            if let Ok(value) = serde_json::to_value(endpoint) {
                groups
                    .entry(endpoint.controller.clone())
                    .or_default()
                    .push(value);
            }
        }
        let project = root
            .trim_end_matches(['/', '\\'])
            .rsplit(['/', '\\'])
            .next()
            .unwrap_or("Project")
            .to_owned();
        let controllers: Vec<Value> = groups
            .into_iter()
            .map(|(name, endpoints)| {
                let first = endpoints.first();
                let source = first
                    .and_then(|endpoint| endpoint.get("source"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let rel = source
                    .strip_prefix(&root)
                    .unwrap_or(source)
                    .trim_start_matches(['/', '\\']);
                let module = request::module_root(source)
                    .map(|module| {
                        module
                            .strip_prefix(&root)
                            .unwrap_or(&module)
                            .trim_matches(['/', '\\'])
                            .to_owned()
                    })
                    .filter(|module| !module.is_empty())
                    .unwrap_or_else(|| project.clone());
                let package = first
                    .and_then(|endpoint| endpoint.get("controllerPackage"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                let description = first
                    .and_then(|endpoint| endpoint.get("controllerDescription"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                json!({
                    "name": name,
                    "rel": rel,
                    "module": module,
                    "controllerPackage": package,
                    "controllerDescription": description,
                    "endpoints": endpoints
                })
            })
            .collect();
        self.post(json!({
            "op": "endpoints",
            "error": null,
            "project": project,
            "controllers": controllers,
            "count": self.endpoints.len(),
        }));
        self.post_environments(ctx);
        self.post_history(&self.history());
        self.post_collections(&self.collections());
        self.post_tabs();
        if let Some(key) = self.pending.clone() {
            let _ = self.select(ctx, &key);
        }
        Ok(())
    }

    fn select(&mut self, ctx: &Invocation, key: &str) -> Result<(), Error> {
        let Some(endpoint) = self.endpoints.iter().find(|endpoint| endpoint.key == key) else {
            self.post(json!({ "op": "error", "message": "That endpoint is no longer in the project. Refresh the list." }));
            return Ok(());
        };
        let project = ctx.project();
        let host = project
            .as_ref()
            .map(|project| request::host_for(project, &endpoint.source))
            .unwrap_or_else(|| "http://localhost:8080".into());
        let body = match (project.as_ref(), endpoint.body_type.as_deref()) {
            (Some(project), Some(body_type)) => {
                skeleton::body_for(project, &endpoint.source, body_type)
            }
            _ if endpoint.body_type.is_some() => "{}".into(),
            _ => String::new(),
        };
        let body_fields = match (project.as_ref(), endpoint.body_type.as_deref()) {
            (Some(project), Some(body_type)) => {
                skeleton::fields_with_comments(project, &endpoint.source, body_type)
            }
            _ => Vec::new(),
        };
        let body_schema = match (project.as_ref(), endpoint.body_type.as_deref()) {
            (Some(project), Some(body_type)) => {
                Some(skeleton::schema_for(project, &endpoint.source, body_type))
            }
            _ => None,
        };
        let mut headers: Vec<Value> = endpoint
            .headers
            .iter()
            .map(|header| json!({ "name": header.name, "value": header.value, "enabled": true }))
            .collect();
        if let Some(produces) = &endpoint.produces
            && !headers.iter().any(|header| {
                header["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case("accept"))
            })
        {
            headers.push(json!({ "name": "Accept", "value": produces, "enabled": true }));
        }
        if endpoint.body_type.is_some()
            && let Some(consumes) = &endpoint.consumes
            && !headers.iter().any(|header| {
                header["name"]
                    .as_str()
                    .is_some_and(|n| n.eq_ignore_ascii_case("content-type"))
            })
        {
            headers.push(json!({ "name": "Content-Type", "value": consumes, "enabled": true }));
        }
        let body_type = if endpoint.body_type.is_some() {
            if endpoint
                .consumes
                .as_deref()
                .is_some_and(|c| c.contains("x-www-form-urlencoded"))
            {
                "form"
            } else if endpoint
                .consumes
                .as_deref()
                .is_some_and(|c| c.contains("xml"))
            {
                "xml"
            } else if endpoint
                .consumes
                .as_deref()
                .is_some_and(|c| c.starts_with("text/"))
            {
                "text"
            } else {
                "json"
            }
        } else {
            "none"
        };
        let path_params: Vec<Value> = request::path_params(&endpoint.path)
            .into_iter()
            .map(|name| json!({ "name": name, "value": "" }))
            .collect();
        let query: Vec<Value> = endpoint
            .query
            .iter()
            .map(|param| json!({ "name": param.name, "value": param.value.clone().unwrap_or_default() }))
            .collect();
        let mut draft = json!({
            "name": format!("{}.{}", endpoint.controller, endpoint.method_name),
            "method": endpoint.http_method,
            "baseUrl": host,
            "path": endpoint.path,
            "paths": endpoint.paths,
            "pathParams": path_params,
            "query": query,
            "headers": headers,
            "bodyType": body_type,
            "body": body,
            "defaultBody": body,
            "bodyFields": body_fields,
            "bodySchema": body_schema,
            "queryHints": endpoint.query.iter().map(|param| &param.name).collect::<Vec<_>>(),
            "headerHints": endpoint.headers.iter().map(|header| &header.name).collect::<Vec<_>>(),
            "pathHints": request::path_params(&endpoint.path),
            "controller": &endpoint.controller,
            "methodName": &endpoint.method_name,
            "reflection": false,
        });
        let cached = self.cache().remove(key);
        if let Some(entry) = &cached
            && entry.draft.is_object()
        {
            for field in [
                "baseUrl",
                "path",
                "pathParams",
                "query",
                "headers",
                "bodyType",
                "body",
            ] {
                if let Some(value) = entry.draft.get(field) {
                    draft[field] = value.clone();
                }
            }
            if !endpoint
                .paths
                .iter()
                .any(|path| draft["path"].as_str() == Some(path.as_str()))
            {
                draft["path"] = json!(endpoint.path);
            }
            draft["method"] = json!(endpoint.http_method);
        }
        ensure_named_rows(
            draft["query"].as_array_mut(),
            endpoint
                .query
                .iter()
                .map(|param| (param.name.clone(), param.value.clone().unwrap_or_default())),
        );
        ensure_named_rows(
            draft["pathParams"].as_array_mut(),
            request::path_params(&endpoint.path)
                .into_iter()
                .map(|name| (name, String::new())),
        );
        ensure_named_rows(
            draft["headers"].as_array_mut(),
            endpoint
                .headers
                .iter()
                .map(|header| (header.name.clone(), header.value.clone())),
        );
        let selected_ok = self.post(json!({ "op": "selected", "key": key }));
        let draft_ok = self.post(json!({ "op": "draft", "draft": draft }));
        if selected_ok && draft_ok {
            self.pending = None;
        }
        if let Some(entry) = cached
            && let Some(response) = entry.response
        {
            self.post(json!({ "op": "cachedResponse", "response": response, "at": entry.at }));
        }
        Ok(())
    }

    fn send(
        &mut self,
        ctx: &Invocation,
        spec: Spec,
        draft: Option<Value>,
        key: Option<String>,
        reflection: Option<Value>,
    ) {
        if let Some(payload) = reflection {
            self.send_reflection(ctx, payload, draft, key);
            return;
        }
        let started = Instant::now();
        let project = ctx.project();
        let mut spec = self.with_cookies(spec);
        if self.runtime_debug(ctx) {
            spec.timeout_ms = Some(request::MAX_TIMEOUT_MS);
        }
        let result =
            request::to_http(&spec, project.as_ref()).and_then(|request| host::fetch(&request));
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                if let (Some(key), Some(draft)) = (key.as_deref(), draft.as_ref()) {
                    self.save_cache(key, Some(draft.clone()), None);
                }
                self.fail(&error);
                return;
            }
        };
        let content_type = response
            .headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("content-type"))
            .map(|(_, value)| value.clone());
        let text_ok = content_type
            .as_deref()
            .map(text_content_type)
            .unwrap_or_else(|| {
                std::str::from_utf8(&response.body).is_ok() && !response.body.contains(&0)
            });
        let binary = !text_ok;
        let truncated = response.body.len() > MAX_BODY;
        let raw = if truncated {
            &response.body[..MAX_BODY]
        } else {
            &response.body[..]
        };
        let body = if binary {
            String::new()
        } else {
            String::from_utf8_lossy(raw).into_owned()
        };
        let body_base64 = Some(request::base64(raw));
        self.store_cookies(&spec.url, &response.headers);
        let elapsed = started.elapsed().as_millis() as u64;
        let response_value = json!({
            "op": "response",
            "status": response.status,
            "headers": response.headers.iter().map(|(name, value)| json!({ "name": name, "value": value })).collect::<Vec<_>>(),
            "body": body,
            "bodyBase64": body_base64,
            "binary": binary,
            "contentType": content_type,
            "size": response.body.len(),
            "ms": elapsed,
            "truncated": truncated,
        });
        self.post(response_value.clone());
        if let Some(key) = key.as_deref() {
            self.save_cache(key, draft.clone(), Some(response_value));
        }
        let name = draft
            .as_ref()
            .and_then(|draft| draft.get("name"))
            .and_then(Value::as_str)
            .unwrap_or(spec.url.as_str());
        let entries = self.record_history(json!({
            "name": name,
            "method": spec.method,
            "url": spec.url,
            "status": response.status,
            "ms": elapsed,
            "at": now_ms(),
            "key": key,
            "draft": draft,
        }));
        self.post_history(&entries);
    }

    fn send_reflection(
        &mut self,
        ctx: &Invocation,
        payload: Value,
        draft: Option<Value>,
        key: Option<String>,
    ) {
        let started = Instant::now();
        let result = self.runtime_endpoint(ctx).and_then(|(_, port, token)| {
            let request = openidea_plugin::HttpRequest {
                method: "POST".into(),
                url: format!("http://127.0.0.1:{port}/invoke"),
                headers: vec![
                    ("Content-Type".into(), "application/json".into()),
                    ("X-OpenIDEA-Token".into(), token),
                ],
                body: Some(payload.to_string().into_bytes()),
                timeout_ms: Some(if self.runtime_debug(ctx) {
                    request::MAX_TIMEOUT_MS as u32
                } else {
                    30_000
                }),
            };
            host::fetch(&request)
        });
        let response = match result {
            Ok(response) => response,
            Err(error) => {
                self.fail(&error);
                return;
            }
        };
        let parsed: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        let ok = parsed["ok"].as_bool().unwrap_or(false);
        let body = if ok {
            serde_json::to_string_pretty(&parsed["result"])
                .unwrap_or_else(|_| parsed["result"].to_string())
        } else {
            parsed["error"]
                .as_str()
                .unwrap_or("Reflection invocation failed.")
                .to_owned()
        };
        let elapsed = started.elapsed().as_millis() as u64;
        let status = if ok { 200 } else { 500 };
        let response_value = json!({
            "op": "response",
            "status": status,
            "headers": if ok { vec![json!({"name":"Content-Type","value":"application/json"})] } else { Vec::<Value>::new() },
            "body": body,
            "contentType": "application/json",
            "size": body.len(),
            "ms": elapsed,
            "truncated": false,
        });
        self.post(response_value.clone());
        if let Some(key) = key.as_deref() {
            self.save_cache(key, draft.clone(), Some(response_value));
        }
        let name = draft
            .as_ref()
            .and_then(|draft| draft.get("name"))
            .and_then(Value::as_str)
            .unwrap_or("reflection");
        let entries = self.record_history(json!({
            "name": name,
            "method": "REFLECT",
            "url": format!("reflection://{}", payload["controller"].as_str().unwrap_or_default()),
            "status": status,
            "ms": elapsed,
            "at": now_ms(),
            "key": key,
            "draft": draft,
        }));
        self.post_history(&entries);
    }

    fn copy(&self, spec: Spec) {
        match request::curl(&spec).and_then(|command| host::copy_to_clipboard(&command)) {
            Ok(()) => {
                self.post(json!({ "op": "status", "message": "The request was copied as a cURL command." }));
            }
            Err(error) => self.fail(&error),
        }
    }

    fn save(&self, ctx: &Invocation, spec: Spec, name: &str) -> Result<(), Error> {
        let Some(project) = ctx.project() else {
            return Err(Error::Failed("Open a project first.".into()));
        };
        let text = request::http_file(&spec, name)?;
        let path = format!(".openidea/api-client/{}.http", request::slug(name));
        project.write_file(&path, &text)?;
        let _ = host::open_location(&path, 0, 0);
        self.post(json!({ "op": "saved", "path": path }));
        Ok(())
    }

    fn message(&mut self, text: &str, ctx: &Invocation) {
        let Ok(value) = serde_json::from_str::<Value>(text) else {
            return;
        };
        let result = match value["op"].as_str().unwrap_or_default() {
            "scan" | "ready" => self.scan(ctx),
            "select" => value["key"]
                .as_str()
                .map_or(Ok(()), |key| self.select(ctx, key)),
            "openSource" => self.open_source(&value),
            "send" => match serde_json::from_value::<Spec>(value["request"].clone()) {
                Ok(spec) => {
                    self.send(
                        ctx,
                        spec,
                        value.get("draft").cloned(),
                        value["key"].as_str().map(str::to_owned),
                        value
                            .get("reflection")
                            .filter(|reflection| reflection.is_object())
                            .cloned(),
                    );
                    Ok(())
                }
                Err(error) => Err(Error::Invalid(format!(
                    "the request is incomplete: {error}"
                ))),
            },
            "copyCurl" => self.with_spec(&value, |spec| {
                self.copy(spec);
                Ok(())
            }),
            "save" => {
                let name = value["name"].as_str().unwrap_or("request").to_owned();
                self.with_spec(&value, |spec| self.save(ctx, spec, &name))
            }
            "openapi" => self.export_openapi(ctx),
            "exportApifox" => self.export_apifox(
                value["projectId"].as_str().unwrap_or_default(),
                value["token"].as_str().unwrap_or_default(),
            ),
            "importCurl" => {
                let text = value["text"].as_str().unwrap_or_default();
                match request::parse_curl(text) {
                    Ok(spec) => {
                        self.post(json!({ "op": "imported", "request": spec }));
                        Ok(())
                    }
                    Err(error) => Err(error),
                }
            }
            "search" => self.search(ctx),
            "runtime" => self.runtime(ctx),
            "triggerSchedule" => self.trigger_schedule(ctx),
            "invokeMethod" => self.invoke_method(ctx, &value),
            "traceToggle" => self.trace(ctx, true, &value),
            "tracePoll" => self.trace(ctx, false, &value),
            "sqlToggle" => self.sql(ctx, true),
            "sqlPoll" => self.sql(ctx, false),
            "scriptOpen" => self.script_toggle(ctx, true),
            "scriptClose" => self.script_toggle(ctx, false),
            "scriptRun" => self.script_run(ctx, value["code"].as_str().unwrap_or_default()),
            "gateway" => self.gateway(ctx),
            "mcpOpen" => self.mcp_open(ctx),
            "mcpRun" => self.mcp_run(
                ctx,
                value["tool"].as_str().unwrap_or_default(),
                value.get("args").cloned().unwrap_or_else(|| json!({})),
            ),
            "selectRuntime" => match value.get("mapping") {
                Some(mapping) => self.select_runtime(ctx, mapping),
                None => Err(Error::Invalid("No runtime mapping was provided.".into())),
            },
            "pickProjectFile" => {
                self.pick_project_file(ctx, value["id"].as_str().unwrap_or_default())
            }
            "saveCollection" => match serde_json::from_value::<Spec>(value["request"].clone()) {
                Ok(request) => {
                    let name = value["name"].as_str().unwrap_or("request").to_owned();
                    let folder = value["folder"].as_str().unwrap_or_default();
                    let entries = self.save_collection(
                        &name,
                        folder,
                        value["draft"].clone(),
                        serde_json::to_value(request).unwrap_or(Value::Null),
                        value["key"].as_str().map(str::to_owned),
                    );
                    self.post_collections(&entries);
                    Ok(())
                }
                Err(error) => Err(Error::Invalid(format!(
                    "the request is incomplete: {error}"
                ))),
            },
            "saveEnvironment" => {
                let name = value["name"].as_str().unwrap_or_default().trim().to_owned();
                if name.is_empty() {
                    return;
                }
                let parse = |key: &str| -> Vec<EnvField> {
                    value[key]
                        .as_array()
                        .map(|rows| {
                            rows.iter()
                                .filter_map(|row| {
                                    let name = row["name"].as_str()?.trim();
                                    if name.is_empty() {
                                        return None;
                                    }
                                    Some(EnvField {
                                        name: name.to_owned(),
                                        value: row["value"].as_str().unwrap_or_default().to_owned(),
                                        enabled: row["enabled"].as_bool().unwrap_or(true),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default()
                };
                let mut all = self.stored_envs();
                all.insert(
                    name,
                    StoredEnv {
                        host: value["host"].as_str().unwrap_or_default().trim().to_owned(),
                        params: parse("params"),
                        headers: parse("headers"),
                        form: parse("form"),
                    },
                );
                if let Ok(text) = serde_json::to_string(&all) {
                    let _ = host::storage::set(ENVS_KEY, &text);
                }
                self.post_environments(ctx);
                Ok(())
            }
            "deleteEnvironment" => {
                let name = value["name"].as_str().unwrap_or_default();
                let mut all = self.stored_envs();
                all.remove(name);
                if let Ok(text) = serde_json::to_string(&all) {
                    let _ = host::storage::set(ENVS_KEY, &text);
                }
                self.post_environments(ctx);
                Ok(())
            }
            "wsConnect" => self.ws_connect(ctx, &value),
            "wsSend" => self.ws_send(&value),
            "wsPoll" => self.ws_poll(),
            "wsClose" => {
                self.socket = None;
                self.post(json!({ "op": "wsClosed" }));
                Ok(())
            }
            "saveTabs" => {
                let mut tabs = value["tabs"].as_array().cloned().unwrap_or_default();
                tabs.truncate(TABS_LIMIT);
                for tab in &mut tabs {
                    if let Some(response) = tab.get_mut("response") {
                        *response = trim_cached_response(response.clone(), 256 * 1024);
                    }
                }
                let stored = json!({ "tabs": tabs, "active": value["active"].clone() });
                if let Ok(text) = serde_json::to_string(&stored) {
                    let _ = host::storage::set(TABS_KEY, &text);
                }
                Ok(())
            }
            "cookies" => {
                self.post(json!({ "op": "cookies", "sites": self.cookies() }));
                Ok(())
            }
            "clearCookies" => {
                let _ = host::storage::delete(COOKIES_KEY);
                self.post(json!({ "op": "status", "message": "Cookie jar cleared." }));
                Ok(())
            }
            "deleteCollection" => {
                let entries = self.delete_collection(value["id"].as_str().unwrap_or_default());
                self.post_collections(&entries);
                Ok(())
            }
            "renameCollection" => {
                let entries = self.rename_collection(
                    value["id"].as_str().unwrap_or_default(),
                    value["name"].as_str().unwrap_or_default(),
                );
                self.post_collections(&entries);
                Ok(())
            }
            "moveCollection" => {
                let entries = self.move_collection(
                    value["id"].as_str().unwrap_or_default(),
                    value["folder"].as_str().unwrap_or_default(),
                );
                self.post_collections(&entries);
                Ok(())
            }
            "runCollection" => {
                let ids: Vec<String> = value["ids"]
                    .as_array()
                    .map(|ids| {
                        ids.iter()
                            .filter_map(Value::as_str)
                            .map(str::to_owned)
                            .collect()
                    })
                    .unwrap_or_default();
                let iterations = value["iterations"].as_u64().unwrap_or(1) as u32;
                self.run_collections(ctx, &ids, iterations);
                Ok(())
            }
            "stress" => match serde_json::from_value::<Spec>(value["request"].clone()) {
                Ok(spec) => {
                    self.stress(ctx, spec, value["count"].as_u64().unwrap_or(20) as u32);
                    Ok(())
                }
                Err(error) => Err(Error::Invalid(format!(
                    "the request is incomplete: {error}"
                ))),
            },
            "saveResponse" => self.save_response(
                ctx,
                value["body"].as_str().unwrap_or_default(),
                value["contentType"].as_str().unwrap_or_default(),
                value["name"].as_str().unwrap_or("response"),
            ),
            "clearHistory" => {
                let _ = host::storage::delete(HISTORY_KEY);
                self.post_history(&[]);
                Ok(())
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            self.fail(&error);
        }
    }

    fn ws_connect(&mut self, ctx: &Invocation, value: &Value) -> Result<(), Error> {
        let Some(project) = ctx.project() else {
            return Err(Error::Failed("Open a project first.".into()));
        };
        let url = value["url"].as_str().unwrap_or_default().trim();
        if !(url.starts_with("ws://") || url.starts_with("wss://")) {
            return Err(Error::Invalid(
                "The WebSocket URL must start with ws:// or wss://.".into(),
            ));
        }
        let headers: Vec<(String, String)> = value["headers"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(|row| {
                        let name = row["name"].as_str()?.trim();
                        if name.is_empty() {
                            return None;
                        }
                        Some((
                            name.to_owned(),
                            row["value"].as_str().unwrap_or_default().to_owned(),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default();
        let request = host::WebSocketRequest {
            url: url.to_owned(),
            headers,
            subprotocol: value["subprotocol"].as_str().unwrap_or_default().to_owned(),
        };
        self.socket = Some(host::WebSocket::connect(&project, &request)?);
        self.post(json!({ "op": "wsConnected", "url": url }));
        Ok(())
    }

    fn ws_send(&self, value: &Value) -> Result<(), Error> {
        let Some(socket) = &self.socket else {
            return Err(Error::Failed("Connect a WebSocket first.".into()));
        };
        let text = value["text"].as_str().unwrap_or_default();
        socket.send(text.as_bytes())?;
        Ok(())
    }

    fn ws_poll(&self) -> Result<(), Error> {
        let Some(socket) = &self.socket else {
            return Ok(());
        };
        let batch = socket.drain(200)?;
        let messages: Vec<Value> = batch
            .messages
            .iter()
            .map(|bytes| match std::str::from_utf8(bytes) {
                Ok(text) => json!({ "text": text, "binary": false }),
                Err(_) => json!({ "text": request::base64(bytes), "binary": true }),
            })
            .collect();
        self.post(json!({ "op": "wsMessages", "messages": messages, "closed": batch.closed }));
        if batch.closed {
            self.post(json!({ "op": "wsClosed" }));
        }
        Ok(())
    }

    fn runtime_debug(&self, ctx: &Invocation) -> bool {
        let Ok((path, _, _)) = self.runtime_endpoint(ctx) else {
            return false;
        };
        let Some(project) = ctx.project() else {
            return false;
        };
        let Ok(text) = project.read_file(&path) else {
            return false;
        };
        serde_json::from_str::<Value>(&text)
            .ok()
            .and_then(|value| value["debug"].as_bool())
            .unwrap_or(false)
    }

    fn runtime_endpoint(&self, ctx: &Invocation) -> Result<(String, u64, String), Error> {
        let Some(project) = ctx.project() else {
            return Err(Error::Failed("Open a project first.".into()));
        };
        let root = project.root()?;
        let mut candidates = vec![format!("{root}/.openidea/spring-agent.json")];
        for endpoint in &self.endpoints {
            if let Some(module) = request::module_root(&endpoint.source) {
                let path = format!("{module}/.openidea/spring-agent.json");
                if !candidates.contains(&path) {
                    candidates.push(path);
                }
            }
        }
        if let Ok(files) = project.find_files("**/.openidea/spring-agent.json", 10) {
            candidates.extend(files);
        }
        let mut found = None;
        for path in candidates {
            if let Ok(text) = project.read_file(&path) {
                found = Some((path, text));
                break;
            }
        }
        let Some((path, text)) = found else {
            return Err(Error::Failed(
                "No runtime agent file. Start the application with the Spring API Client agent injected."
                    .into(),
            ));
        };
        let value: Value = serde_json::from_str(&text)
            .map_err(|error| Error::Invalid(format!("invalid runtime file: {error}")))?;
        let port = value["port"].as_u64().unwrap_or(0);
        if port == 0 {
            return Err(Error::Failed("The runtime agent has no port yet.".into()));
        }
        let token = value["token"].as_str().unwrap_or_default().to_owned();
        Ok((path, port, token))
    }

    fn trigger_schedule(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let (_, port, token) = self.runtime_endpoint(ctx)?;
        let schedules = self.agent_get(port, &token, "/schedules")?;
        let items: Vec<PickItem> = schedules["schedules"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|schedule| {
                let class = schedule["className"].as_str()?;
                let method = schedule["methodName"].as_str()?;
                let bean = schedule["bean"].as_str().unwrap_or("");
                Some(
                    PickItem::new(
                        &format!("{class}|{method}|{bean}"),
                        &format!("{class}.{method}"),
                    )
                    .description(bean),
                )
            })
            .collect();
        if items.is_empty() {
            return Err(Error::Failed(
                "The runtime agent reported no @Scheduled methods.".into(),
            ));
        }
        let Some(key) = host::pick("Trigger a scheduled method", &items)? else {
            return Ok(());
        };
        let mut parts = key.split('|');
        let class = parts.next().unwrap_or_default();
        let method = parts.next().unwrap_or_default();
        let bean = parts.next().unwrap_or_default();
        let request = openidea_plugin::HttpRequest {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/triggerSchedule"),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-OpenIDEA-Token".into(), token),
            ],
            body: Some(
                json!({ "className": class, "methodName": method, "bean": bean })
                    .to_string()
                    .into_bytes(),
            ),
            timeout_ms: Some(30_000),
        };
        let response = host::fetch(&request)?;
        let value: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        if value["ok"].as_bool().unwrap_or(false) {
            self.post(json!({
                "op": "status",
                "message": format!("Triggered {class}.{method}: {}", value["result"]),
            }));
        } else {
            return Err(Error::Failed(
                value["error"]
                    .as_str()
                    .unwrap_or("Scheduled method failed.")
                    .into(),
            ));
        }
        Ok(())
    }

    fn trace(&mut self, ctx: &Invocation, toggle: bool, value: &Value) -> Result<(), Error> {
        if toggle {
            self.trace_targets = value["targets"].as_array().cloned().unwrap_or_default();
        }
        let (_, port, token) = self.runtime_endpoint(ctx)?;
        let current = self.agent_get(port, &token, "/trace")?;
        let enabled = if toggle {
            !current["enabled"].as_bool().unwrap_or(false)
        } else {
            current["enabled"].as_bool().unwrap_or(false)
        };
        let request = openidea_plugin::HttpRequest {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/trace"),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-OpenIDEA-Token".into(), token),
            ],
            body: Some(if toggle {
                json!({
                    "enabled": enabled,
                    "targets": value.get("targets").cloned().unwrap_or_else(|| json!([])),
                    "maxDepth": value.get("maxDepth").and_then(Value::as_u64).unwrap_or(8),
                    "includeSql": value.get("includeSql").and_then(Value::as_bool).unwrap_or(true),
                })
            } else {
                json!({ "enabled": enabled })
            }
            .to_string()
            .into_bytes()),
            timeout_ms: Some(5_000),
        };
        let response = host::fetch(&request)?;
        let value: Value = serde_json::from_slice(&response.body)
            .unwrap_or_else(|_| json!({ "enabled": enabled, "frames": [] }));
        self.post(json!({
            "op": "trace",
            "enabled": value["enabled"].as_bool().unwrap_or(enabled),
            "trees": value.get("trees").cloned().unwrap_or_else(|| json!([])),
            "frames": value.get("frames").cloned().unwrap_or_else(|| json!([])),
        }));
        Ok(())
    }

    fn sql(&mut self, ctx: &Invocation, toggle: bool) -> Result<(), Error> {
        let (_, port, token) = self.runtime_endpoint(ctx)?;
        let current = self.agent_get(port, &token, "/sql")?;
        let enabled = if toggle {
            !current["enabled"].as_bool().unwrap_or(false)
        } else {
            current["enabled"].as_bool().unwrap_or(false)
        };
        let request = openidea_plugin::HttpRequest {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/sql"),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-OpenIDEA-Token".into(), token),
            ],
            body: Some(json!({ "enabled": enabled }).to_string().into_bytes()),
            timeout_ms: Some(5_000),
        };
        let response = host::fetch(&request)?;
        let value: Value = serde_json::from_slice(&response.body)
            .unwrap_or_else(|_| json!({ "enabled": enabled, "frames": [] }));
        self.post(json!({
            "op": "sql",
            "enabled": value["enabled"].as_bool().unwrap_or(enabled),
            "frames": value.get("frames").cloned().unwrap_or_else(|| json!([])),
        }));
        Ok(())
    }

    fn script_toggle(&mut self, ctx: &Invocation, enabled: bool) -> Result<(), Error> {
        let (_, port, token) = self.runtime_endpoint(ctx)?;
        let request = openidea_plugin::HttpRequest {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/script"),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-OpenIDEA-Token".into(), token),
            ],
            body: Some(json!({ "enabled": enabled }).to_string().into_bytes()),
            timeout_ms: Some(5_000),
        };
        let response = host::fetch(&request)?;
        let value: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        self.post(json!({
            "op": "scriptState",
            "enabled": value["enabled"].as_bool().unwrap_or(enabled),
        }));
        Ok(())
    }

    fn script_run(&mut self, ctx: &Invocation, code: &str) -> Result<(), Error> {
        let (_, port, token) = self.runtime_endpoint(ctx)?;
        let enable = openidea_plugin::HttpRequest {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/script"),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-OpenIDEA-Token".into(), token.clone()),
            ],
            body: Some(json!({ "enabled": true }).to_string().into_bytes()),
            timeout_ms: Some(5_000),
        };
        host::fetch(&enable)?;
        let request = openidea_plugin::HttpRequest {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/script/run"),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-OpenIDEA-Token".into(), token),
            ],
            body: Some(
                json!({ "code": code, "timeoutMs": 10_000 })
                    .to_string()
                    .into_bytes(),
            ),
            timeout_ms: Some(15_000),
        };
        let response = host::fetch(&request)?;
        let value: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        self.post(json!({
            "op": "scriptResult",
            "ok": value["ok"].as_bool().unwrap_or(false),
            "result": value["result"],
            "error": value["error"],
            "ms": value["ms"],
        }));
        Ok(())
    }

    fn gateway(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let (_, port, token) = self.runtime_endpoint(ctx)?;
        let value = self.agent_get(port, &token, "/routes")?;
        self.post(json!({
            "op": "gateway",
            "routes": value.get("routes").cloned().unwrap_or_else(|| json!([])),
            "error": value["error"],
        }));
        Ok(())
    }

    fn mcp_open(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let (_, port, token) = self.runtime_endpoint(ctx)?;
        let request = openidea_plugin::HttpRequest {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/mcp"),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-OpenIDEA-Token".into(), token.clone()),
            ],
            body: Some(
                json!({ "jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {} })
                    .to_string()
                    .into_bytes(),
            ),
            timeout_ms: Some(5_000),
        };
        let response = host::fetch(&request)?;
        let value: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        self.post(json!({
            "op": "mcp",
            "endpoint": format!("http://127.0.0.1:{port}/mcp"),
            "token": token,
            "tools": value["result"]["tools"].as_array().cloned().unwrap_or_default(),
            "error": value["error"]["message"],
        }));
        Ok(())
    }

    fn mcp_run(&mut self, ctx: &Invocation, tool: &str, args: Value) -> Result<(), Error> {
        let (_, port, token) = self.runtime_endpoint(ctx)?;
        let request = openidea_plugin::HttpRequest {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/mcp"),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-OpenIDEA-Token".into(), token),
            ],
            body: Some(
                json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "tools/call",
                    "params": { "name": tool, "arguments": args },
                })
                .to_string()
                .into_bytes(),
            ),
            timeout_ms: Some(30_000),
        };
        let response = host::fetch(&request)?;
        let value: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        let text = value["result"]["content"][0]["text"]
            .as_str()
            .map(str::to_owned)
            .or_else(|| value["error"]["message"].as_str().map(str::to_owned))
            .unwrap_or_else(|| value.to_string());
        self.post(json!({
            "op": "mcpResult",
            "ok": value["error"].is_null(),
            "text": text,
        }));
        Ok(())
    }

    fn agent_get(&self, port: u64, token: &str, path: &str) -> Result<Value, Error> {
        let request = openidea_plugin::HttpRequest {
            method: "GET".into(),
            url: format!("http://127.0.0.1:{port}{path}"),
            headers: vec![("X-OpenIDEA-Token".into(), token.to_owned())],
            body: None,
            timeout_ms: Some(3_000),
        };
        let response = host::fetch(&request)?;
        serde_json::from_slice(&response.body)
            .map_err(|error| Error::Invalid(format!("agent returned invalid JSON: {error}")))
    }

    fn select_runtime(&mut self, ctx: &Invocation, mapping: &Value) -> Result<(), Error> {
        let Some(project) = ctx.project() else {
            return Err(Error::Failed("Open a project first.".into()));
        };
        let (runtime_path, _, _) = self.runtime_endpoint(ctx)?;
        let runtime: Value = serde_json::from_str(&project.read_file(&runtime_path)?)
            .map_err(|error| Error::Invalid(format!("invalid runtime file: {error}")))?;
        let port = runtime["serverPort"].as_u64().unwrap_or(8080);
        let context_path = runtime["contextPath"].as_str().unwrap_or_default();
        let controller = mapping["controller"].as_str().unwrap_or("Runtime");
        let method_name = mapping["methodName"].as_str().unwrap_or("invoke");
        let method = mapping["method"]
            .as_str()
            .unwrap_or("GET")
            .to_ascii_uppercase();
        let path = mapping["path"].as_str().unwrap_or("/");
        let key = format!("runtime:{controller}:{method_name}:{path}");
        let path_params: Vec<Value> = request::path_params(path)
            .into_iter()
            .map(|name| json!({ "name": name, "value": "" }))
            .collect();
        let draft = json!({
            "name": format!("{controller}.{method_name}"),
            "method": method,
            "baseUrl": format!("http://localhost:{port}{context_path}"),
            "path": path,
            "paths": [path],
            "pathParams": path_params,
            "query": [],
            "headers": [],
            "bodyType": "none",
            "body": "",
            "controller": controller,
            "methodName": method_name,
            "reflection": false,
            "timeoutMs": 10000,
        });
        self.post(json!({ "op": "selected", "key": key }));
        self.post(json!({ "op": "draft", "draft": draft }));
        Ok(())
    }

    fn runtime(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let (path, port, token) = self.runtime_endpoint(ctx)?;
        let request = openidea_plugin::HttpRequest {
            method: "GET".into(),
            url: format!("http://127.0.0.1:{port}/health"),
            headers: vec![("X-OpenIDEA-Token".into(), token.clone())],
            body: None,
            timeout_ms: Some(2_000),
        };
        let response = host::fetch(&request)?;
        let mapping_value = self
            .agent_get(port, &token, "/mappings")
            .unwrap_or(Value::Null);
        let mappings = mapping_value["mappings"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let schedules = self
            .agent_get(port, &token, "/schedules")
            .ok()
            .and_then(|value| value["schedules"].as_array().map(|items| items.len()))
            .unwrap_or(0);
        let route_value = self
            .agent_get(port, &token, "/routes")
            .unwrap_or(Value::Null);
        let routes = route_value["routes"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let runtime: Value = serde_json::from_str(
            &ctx.project()
                .and_then(|project| project.read_file(&path).ok())
                .unwrap_or_default(),
        )
        .unwrap_or(Value::Null);
        self.post(json!({
            "op": "runtime",
            "path": path,
            "pid": runtime["pid"],
            "port": port,
            "context": runtime["context"],
            "status": response.status,
            "mappings": mappings.len(),
            "runtimeMappings": mappings,
            "serverPort": runtime["serverPort"],
            "contextPath": runtime["contextPath"],
            "schedules": schedules,
            "routes": routes.len(),
            "runtimeRoutes": routes,
            "debug": self.runtime_debug(ctx),
        }));
        Ok(())
    }

    fn open_source(&mut self, value: &Value) -> Result<(), Error> {
        let source = value["source"].as_str().unwrap_or_default();
        let line = value["line"].as_u64().unwrap_or(0) as u32;
        if source.is_empty() {
            return Err(Error::Failed(
                "This runtime endpoint has no source location.".into(),
            ));
        }
        host::open_location(source, line, 0)?;
        self.post(json!({
            "op": "status",
            "message": format!("Opened {}:{}", source, line + 1),
        }));
        Ok(())
    }

    fn search(&mut self, ctx: &Invocation) -> Result<(), Error> {
        if self.endpoints.is_empty() {
            self.scan(ctx)?;
        }
        let items: Vec<PickItem> = self
            .endpoints
            .iter()
            .map(|endpoint| {
                PickItem::new(
                    &endpoint.key,
                    &format!("{} {}", endpoint.http_method, endpoint.path),
                )
                .description(&format!("{}.{}", endpoint.controller, endpoint.method_name))
            })
            .collect();
        if items.is_empty() {
            return Err(Error::Failed("No Spring endpoints were found.".into()));
        }
        let Some(key) = host::pick("Search Spring API", &items)? else {
            return Ok(());
        };
        self.open_endpoint(ctx, &key)
    }

    fn pick_project_file(&self, ctx: &Invocation, id: &str) -> Result<(), Error> {
        let Some(project) = ctx.project() else {
            return Err(Error::Failed("Open a project first.".into()));
        };
        let root = project.root().unwrap_or_default();
        let mut files: Vec<String> = project.find_files("**/*", 1000)?;
        files.retain(|path| {
            !path.contains("/target/")
                && !path.contains("/build/")
                && !path.contains("/node_modules/")
                && !path.contains("/.git/")
        });
        files.sort();
        files.truncate(500);
        if files.is_empty() {
            return Err(Error::Failed("No files were found in the project.".into()));
        }
        let items: Vec<PickItem> = files
            .iter()
            .map(|path| {
                let rel = path
                    .strip_prefix(&root)
                    .unwrap_or(path)
                    .trim_start_matches(['/', '\\']);
                PickItem::new(path, rel)
            })
            .collect();
        if let Some(path) = host::pick("Choose a project file", &items)? {
            self.post(json!({ "op": "pickedFile", "id": id, "path": path }));
        }
        Ok(())
    }

    fn save_response(
        &self,
        ctx: &Invocation,
        body: &str,
        content_type: &str,
        name: &str,
    ) -> Result<(), Error> {
        let Some(project) = ctx.project() else {
            return Err(Error::Failed("Open a project first.".into()));
        };
        let extension = if content_type.contains("json") {
            "json"
        } else if content_type.contains("xml") {
            "xml"
        } else if content_type.contains("html") {
            "html"
        } else if content_type.contains("css") {
            "css"
        } else if content_type.contains("javascript") {
            "js"
        } else {
            "txt"
        };
        let path = format!(
            ".openidea/api-client/responses/{}-{}.{}",
            request::slug(name),
            now_ms(),
            extension
        );
        project.write_file(&path, body)?;
        let _ = host::open_location(&path, 0, 0);
        self.post(json!({ "op": "saved", "path": path, "kind": "response" }));
        Ok(())
    }

    fn open_endpoint(&mut self, ctx: &Invocation, key: &str) -> Result<(), Error> {
        if !self.endpoints.iter().any(|endpoint| endpoint.key == key)
            && let Some(editor) = ctx.editor()
            && let Ok(document) = editor.document()
        {
            self.endpoints
                .extend(parser::parse_source(&editor.path(), &document.text));
        }
        self.pending = Some(key.to_owned());
        host::reveal_view(VIEW)?;
        self.select(ctx, key)
    }

    fn select_at_caret(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let editor = ctx
            .editor()
            .ok_or_else(|| Error::Failed("Focus a controller method first.".into()))?;
        let path = editor.path().replace('\\', "/");
        let line = editor
            .selections()
            .ok()
            .and_then(|ranges| ranges.first().map(|range| range.start.line))
            .unwrap_or(0);
        if self.endpoints.is_empty() {
            self.scan(ctx)?;
        }
        let key = self
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.source.replace('\\', "/") == path)
            .min_by_key(|endpoint| (i64::from(endpoint.method_line) - i64::from(line)).abs())
            .map(|endpoint| endpoint.key.clone())
            .ok_or_else(|| Error::Failed("No Spring endpoint in this file.".into()))?;
        self.open_endpoint(ctx, &key)
    }

    fn call_method(&mut self, ctx: &Invocation, arg: Option<&str>) -> Result<Option<String>, Error> {
        let Some(arg) = arg else {
            return Err(Error::Invalid("No method was selected.".into()));
        };
        let method: Value = serde_json::from_str(arg)
            .map_err(|error| Error::Invalid(format!("invalid method target: {error}")))?;
        host::reveal_view(VIEW)?;
        self.post(json!({ "op": "callMethod", "method": method }));
        Ok(None)
    }

    fn invoke_method(&mut self, ctx: &Invocation, value: &Value) -> Result<(), Error> {
        let className = value["className"].as_str().unwrap_or_default();
        let methodName = value["methodName"].as_str().unwrap_or_default();
        let result = (|| -> Result<Value, Error> {
            let (_, port, token) = self.runtime_endpoint(ctx)?;
            let payload = json!({
                "className": className,
                "methodName": methodName,
                "parameterTypes": value.get("parameterTypes").cloned().unwrap_or_else(|| json!([])),
                "args": value.get("args").cloned().unwrap_or_else(|| json!([])),
            });
            let request = openidea_plugin::HttpRequest {
                method: "POST".into(),
                url: format!("http://127.0.0.1:{port}/method/invoke"),
                headers: vec![
                    ("Content-Type".into(), "application/json".into()),
                    ("X-OpenIDEA-Token".into(), token),
                ],
                body: Some(payload.to_string().into_bytes()),
                timeout_ms: Some(if self.runtime_debug(ctx) { request::MAX_TIMEOUT_MS as u32 } else { 30_000 }),
            };
            let response = host::fetch(&request)?;
            serde_json::from_slice::<Value>(&response.body)
                .map_err(|error| Error::Invalid(format!("invalid agent response: {error}")))
        })();
        match result {
            Ok(parsed) => {
                self.post(json!({
                    "op": "methodResult",
                    "className": className,
                    "methodName": methodName,
                    "ok": parsed["ok"].as_bool().unwrap_or(false),
                    "result": parsed.get("result").cloned().unwrap_or(Value::Null),
                    "error": parsed.get("error").cloned().unwrap_or(Value::Null),
                    "bean": parsed.get("bean").cloned().unwrap_or(Value::Null),
                    "ms": parsed.get("ms").cloned().unwrap_or(Value::Null),
                }));
            }
            Err(error) => {
                self.post(json!({
                    "op": "methodResult",
                    "className": className,
                    "methodName": methodName,
                    "ok": false,
                    "error": error.to_string(),
                }));
            }
        }
        Ok(())
    }

    fn trace_at_caret(&mut self, ctx: &Invocation) -> Result<(), Error> {
        let editor = ctx
            .editor()
            .ok_or_else(|| Error::Failed("Focus a Java method first.".into()))?;
        let document = editor
            .document()
            .map_err(|_| Error::Failed("Cannot read the focused editor.".into()))?;
        let line = editor
            .selections()
            .ok()
            .and_then(|ranges| ranges.first().map(|range| range.start.line))
            .unwrap_or(0);
        let (type_name, method) = parser::method_at_caret(&document.text, line)
            .ok_or_else(|| Error::Failed("No Java method at the caret.".into()))?;
        let (_, port, token) = self.runtime_endpoint(ctx)?;
        let target = json!({ "type": type_name, "method": method, "entry": false });
        if let Some(index) = self.trace_targets.iter().position(|item| item == &target) {
            self.trace_targets.remove(index);
        } else {
            self.trace_targets.push(target);
        }
        let enabled = !self.trace_targets.is_empty();
        let request = openidea_plugin::HttpRequest {
            method: "POST".into(),
            url: format!("http://127.0.0.1:{port}/trace"),
            headers: vec![
                ("Content-Type".into(), "application/json".into()),
                ("X-OpenIDEA-Token".into(), token),
            ],
            body: Some(
                json!({ "enabled": enabled, "targets": self.trace_targets })
                    .to_string()
                    .into_bytes(),
            ),
            timeout_ms: Some(5_000),
        };
        let response = host::fetch(&request)?;
        let value: Value = serde_json::from_slice(&response.body).unwrap_or(Value::Null);
        self.post(json!({
            "op": "trace",
            "enabled": value["enabled"].as_bool().unwrap_or(enabled),
            "trees": value.get("trees").cloned().unwrap_or_else(|| json!([])),
            "targets": self.trace_targets,
        }));
        host::reveal_view(VIEW)?;
        Ok(())
    }

    fn export_apifox(&self, project_id: &str, token: &str) -> Result<(), Error> {
        let project_id = project_id.trim();
        let token = token.trim();
        if project_id.is_empty() || token.is_empty() {
            return Err(Error::Invalid(
                "Apifox project ID and token are required.".into(),
            ));
        }
        if self.endpoints.is_empty() {
            return Err(Error::Failed("Scan the project before exporting.".into()));
        }
        let document = openapi::document(&self.endpoints, "Spring API Client");
        let input =
            serde_json::to_string(&document).map_err(|error| Error::Invalid(error.to_string()))?;
        let body = json!({
            "input": input,
            "options": {
                "endpointOverwriteBehavior": "AUTO_MERGE",
                "schemaOverwriteBehavior": "KEEP_EXISTING"
            }
        });
        let request = openidea_plugin::HttpRequest {
            method: "POST".into(),
            url: format!(
                "https://api.apifox.com/v1/projects/{project_id}/import-openapi?locale=zh-CN"
            ),
            headers: vec![
                ("Authorization".into(), format!("Bearer {token}")),
                ("X-Apifox-Api-Version".into(), "2024-03-28".into()),
                ("Content-Type".into(), "application/json".into()),
            ],
            body: Some(body.to_string().into_bytes()),
            timeout_ms: Some(30_000),
        };
        let response = host::fetch(&request)?;
        let text = String::from_utf8_lossy(&response.body);
        if response.status >= 400 {
            return Err(Error::Failed(format!(
                "Apifox returned HTTP {}: {}",
                response.status,
                text.trim()
            )));
        }
        let parsed: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let counters = &parsed["data"]["counters"];
        self.post(json!({
            "op": "apifoxDone",
            "status": response.status,
            "created": counters["endpointCreated"],
            "updated": counters["endpointUpdated"],
            "failed": counters["endpointFailed"],
        }));
        Ok(())
    }

    fn export_openapi(&self, ctx: &Invocation) -> Result<(), Error> {
        let Some(project) = ctx.project() else {
            return Err(Error::Failed("Open a project first.".into()));
        };
        let root = project.root()?;
        let title = root
            .rsplit(['/', '\\'])
            .next()
            .filter(|title| !title.is_empty())
            .unwrap_or("API");
        let document = openapi::document(&self.endpoints, title);
        let text = serde_json::to_string_pretty(&document)
            .map_err(|error| Error::Failed(format!("cannot write OpenAPI: {error}")))?;
        let path = ".openidea/api-client/openapi.json";
        project.write_file(path, &text)?;
        let _ = host::open_location(path, 0, 0);
        self.post(json!({ "op": "saved", "path": path, "kind": "openapi" }));
        Ok(())
    }

    fn with_spec(
        &self,
        value: &Value,
        action: impl FnOnce(Spec) -> Result<(), Error>,
    ) -> Result<(), Error> {
        let spec = serde_json::from_value::<Spec>(value["request"].clone())
            .map_err(|error| Error::Invalid(format!("the request is incomplete: {error}")))?;
        action(spec)
    }
}

fn text_content_type(content_type: &str) -> bool {
    let content_type = content_type.to_ascii_lowercase();
    content_type.starts_with("text/")
        || content_type.contains("json")
        || content_type.contains("xml")
        || content_type.contains("javascript")
        || content_type.contains("x-www-form-urlencoded")
        || content_type.contains("graphql")
}

fn trim_cached_response(mut response: Value, limit: usize) -> Value {
    for field in ["body", "bodyBase64"] {
        if let Some(text) = response[field].as_str()
            && text.len() > limit
        {
            let mut cut = limit;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            response[field] = json!(&text[..cut]);
            response["truncated"] = json!(true);
        }
    }
    response
}

fn url_host(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split('/').next().unwrap_or(rest);
    let authority = authority.rsplit('@').next().unwrap_or(authority);
    let host = authority.split(':').next().unwrap_or(authority);
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

fn ensure_named_rows(
    rows: Option<&mut Vec<serde_json::Value>>,
    defaults: impl Iterator<Item = (String, String)>,
) {
    let Some(rows) = rows else {
        return;
    };
    for (name, value) in defaults {
        if name.is_empty() {
            continue;
        }
        let exists = rows
            .iter()
            .any(|row| row["name"].as_str() == Some(name.as_str()));
        if !exists {
            rows.push(json!({ "name": name, "value": value, "enabled": true }));
        }
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

impl Plugin for ApiClient {
    fn run_command(
        &mut self,
        id: &str,
        ctx: &Invocation,
        _arg: Option<&str>,
    ) -> Result<Option<String>, Error> {
        match id {
            "show" => {
                host::reveal_view(VIEW)?;
                Ok(None)
            }
            "refresh" => {
                self.scan(ctx)?;
                Ok(None)
            }
            "select-at-caret" => self.select_at_caret(ctx).map(|()| None),
            "trace-at-caret" => self.trace_at_caret(ctx).map(|()| None),
            "call-method" => self.call_method(ctx, _arg),
            "search-api" => self.search(ctx).map(|()| None),
            "trigger-schedule" => self.trigger_schedule(ctx).map(|()| None),
            "trace" => self.trace(ctx, true, &Value::Null).map(|()| None),
            "sql" => self.sql(ctx, true).map(|()| None),
            "script" => self.script_toggle(ctx, true).map(|()| None),
            "mcp" => self.mcp_open(ctx).map(|()| None),
            "gateway" => self.gateway(ctx).map(|()| None),
            "open-endpoint" => {
                let key = _arg.ok_or_else(|| Error::Failed("No endpoint to open.".into()))?;
                self.open_endpoint(ctx, key).map(|()| None)
            }
            _ => Err(Error::NotFound(format!("command {id}"))),
        }
    }
}

impl LanguageFeatures for ApiClient {
    fn gutter_actions(&mut self, doc: &Document) -> Vec<GutterAction> {
        let mut lines = std::collections::HashSet::new();
        let mut actions: Vec<GutterAction> = parser::parse_source(&doc.path, &doc.text)
            .into_iter()
            .filter(|endpoint| lines.insert(endpoint.method_line))
            .map(|endpoint| GutterAction {
                line: endpoint.method_line,
                icon: Some("icons/hummingbird.svg".into()),
                tooltip: format!(
                    "Open {} {} in Spring API Client",
                    endpoint.http_method, endpoint.path
                ),
                command: "open-endpoint".into(),
                argument: Some(endpoint.key),
            })
            .collect();
        actions
    }
}

impl Views for ApiClient {
    fn render(
        &mut self,
        view: &str,
        _viewport: Option<openidea_plugin::views::Viewport>,
        _ctx: &Invocation,
    ) -> Result<View, Error> {
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        let script = BODY_EDITOR_JS.replace("</script>", "<\\/script>");
        let html = PAGE.replace(
            "<!--BODY_EDITOR_SCRIPT-->",
            &format!("<script>{script}</script>"),
        );
        Ok(View::new(Body::Html(html)))
    }

    fn handle(&mut self, view: &str, event: ViewEvent, ctx: &Invocation) -> Result<(), Error> {
        if view != VIEW {
            return Err(Error::NotFound(view.into()));
        }
        match event {
            ViewEvent::Message(text) => self.message(&text, ctx),
            ViewEvent::Button(button) if button == "refresh" => {
                if let Err(error) = self.scan(ctx) {
                    self.fail(&error);
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl HoverActions for ApiClient {
    fn hover_actions(&mut self, doc: &Document) -> Vec<HoverAction> {
        parser::parse_callable_methods(&doc.path, &doc.text)
            .into_iter()
            .filter(|method| method.method_name != method.class_name)
            .map(|method| HoverAction {
                line: method.line,
                start_line: method.line,
                end_line: method.end_line,
                icon: Some("run".into()),
                label: "Call this method".to_string(),
                tooltip: Some("Call this method".to_string()),
                command: "call-method".into(),
                argument: Some(json!({
                    "source": method.source,
                    "className": method.class_name_fq,
                    "simpleClassName": method.class_name,
                    "methodName": method.method_name,
                    "parameterTypes": method.parameter_types,
                    "parameterNames": method.parameter_names,
                    "returnType": method.return_type,
                    "line": method.line,
                }).to_string()),
            })
            .collect()
    }
}

register_plugin!(ApiClient, language_features, views, hover_actions);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caches_the_last_draft_and_response_per_endpoint() {
        let _host = openidea_plugin::mock::MockHost::install();
        let client = ApiClient::default();
        client.save_cache(
            "endpoint-key",
            Some(json!({ "method": "POST", "body": "{\"a\":1}" })),
            Some(json!({ "status": 200, "body": "ok" })),
        );
        let cache = client.cache();
        let entry = cache.get("endpoint-key").unwrap();
        assert_eq!(entry.draft["method"], "POST");
        assert_eq!(entry.response.as_ref().unwrap()["status"], 200);
        let _ = host::storage::delete(CACHE_KEY);
    }

    #[test]
    fn opens_the_mapping_source_location() {
        let host = openidea_plugin::mock::MockHost::install();
        let mut client = ApiClient::default();
        client
            .open_source(&json!({ "source": "/p/src/C.java", "line": 12 }))
            .unwrap();
        assert_eq!(
            host.opened_locations(),
            vec![("/p/src/C.java".to_owned(), 12, 0)]
        );
    }

    #[test]
    fn replays_set_cookie_values_on_the_next_request() {
        use std::sync::{Arc, Mutex};
        let host = openidea_plugin::mock::MockHost::install().with_capabilities(&[
            "project.read",
            "storage",
            "net:localhost",
        ]);
        host.set_project_root(Some("/p"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        host.on_fetch(move |request| {
            recorded.lock().unwrap().push(request.clone());
            Ok(openidea_plugin::HttpResponse {
                status: 200,
                headers: vec![("Set-Cookie".into(), "sid=abc; Path=/".into())],
                body: b"ok".to_vec(),
            })
        });
        let ctx = host.invocation();
        let spec = || Spec {
            method: "GET".into(),
            url: "http://localhost:8088/hello".into(),
            headers: Vec::new(),
            body: None,
            body_base64: None,
            multipart: Vec::new(),
            timeout_ms: None,
        };
        let mut client = ApiClient::default();
        client.send(&ctx, spec(), None, None, None);
        client.send(&ctx, spec(), None, None, None);
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert!(
            seen[1]
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("cookie") && value == "sid=abc"),
            "headers: {:?}",
            seen[1].headers
        );
        let _ = host::storage::delete(COOKIES_KEY);
    }

    #[test]
    fn extracts_cookie_hosts() {
        assert_eq!(
            url_host("http://localhost:8088/api?x=1").as_deref(),
            Some("localhost")
        );
        assert_eq!(
            url_host("https://user:pass@example.com/x").as_deref(),
            Some("example.com")
        );
        assert_eq!(url_host("not a url"), None);
    }

    #[test]
    fn saves_and_deletes_collections() {
        let _host = openidea_plugin::mock::MockHost::install();
        let client = ApiClient::default();
        let entries = client.save_collection(
            "hello",
            "",
            json!({ "name": "HelloController.hello", "method": "GET", "path": "/hello" }),
            json!({ "method": "GET", "url": "http://localhost:8088/hello", "headers": [] }),
            None,
        );
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "hello");
        let entries = client.rename_collection(&entries[0].id, "hello-renamed");
        assert_eq!(entries[0].name, "hello-renamed");
        let entries = client.move_collection(&entries[0].id, "smoke");
        assert_eq!(entries[0].folder, "smoke");
        let remaining = client.delete_collection(&entries[0].id);
        assert!(remaining.is_empty());
        let _ = host::storage::delete(COLLECTIONS_KEY);
    }

    #[test]
    fn keeps_the_last_requests_in_storage() {
        let _host = openidea_plugin::mock::MockHost::install();
        let client = ApiClient::default();
        client.record_history(json!({ "name": "first" }));
        client.record_history(json!({ "name": "second" }));
        assert_eq!(client.history().len(), 2);
        assert_eq!(client.history()[0]["name"], "second");
        for index in 0..40 {
            client.record_history(json!({ "name": index }));
        }
        assert_eq!(client.history().len(), HISTORY_LIMIT);
        let _ = host::storage::delete(HISTORY_KEY);
        assert!(client.history().is_empty());
    }

    #[test]
    fn offers_a_gutter_action_per_mapping() {
        let text = r#"
@RestController
@RequestMapping("address")
public class AddressController {
    @PostMapping("/list")
    public String list(@RequestParam String userId) { return null; }
    @PostMapping("/add")
    public String add(@RequestBody Object body) { return null; }
}
"#;
        let document = Document {
            path: "/p/AddressController.java".into(),
            language: "java".into(),
            version: 1,
            text: text.into(),
        };
        let mut client = ApiClient::default();
        let actions = client.gutter_actions(&document);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].line, 5);
        assert_eq!(actions[1].line, 7);
        assert_eq!(actions[0].command, "open-endpoint");
        assert_eq!(actions[0].icon.as_deref(), Some("icons/hummingbird.svg"));
        assert!(
            actions[0]
                .argument
                .as_deref()
                .is_some_and(|key| key.contains("AddressController"))
        );
        let calls = client.hover_actions(&document);
        assert_eq!(calls.len(), 2);
        let call = calls.iter().find(|a| a.command == "call-method").expect("call method action");
        let target: serde_json::Value = serde_json::from_str(call.argument.as_deref().unwrap()).unwrap();
        assert_eq!(target["className"], "AddressController");
        assert_eq!(target["methodName"], "list");
        assert_eq!(target["parameterTypes"], serde_json::json!(["String"]));
        assert_eq!(call.icon.as_deref(), Some("run"));
        assert_eq!(call.line, 5);
        assert_eq!(call.start_line, 5);
        assert_eq!(call.end_line, 5);
    }

    #[test]
    fn selects_the_endpoint_at_the_caret_when_the_action_runs() {
        let host = openidea_plugin::mock::MockHost::install();
        host.set_project_root(Some("/p"));
        let path = "/p/AddressController.java";
        let text = r#"
@RestController
@RequestMapping("address")
public class AddressController {
    @PostMapping("/list")
    public String list(@RequestParam String userId) { return null; }
}
"#;
        host.open_editor(path, "java", text, &[(5, 4, 5, 8)]);
        let endpoints = parser::parse_source(path, text);
        let mut client = ApiClient {
            endpoints,
            pending: None,
            socket: None,
            trace_targets: Vec::new(),
        };
        client.select_at_caret(&host.invocation()).unwrap();
        assert!(
            host.view_messages()
                .iter()
                .any(|(view, message)| view == VIEW && message.contains("\"op\":\"selected\"")),
            "messages: {:?}",
            host.view_messages()
        );
    }

    #[test]
    fn serializes_a_scan_result_for_the_page() {
        let endpoints = parser::parse_source(
            "/p/UserController.java",
            r#"
@RestController
@RequestMapping("/api")
public class UserController {
    @GetMapping("/ping")
    public String ping() { return "pong"; }
}
"#,
        );
        assert_eq!(endpoints.len(), 1);
        assert_eq!(endpoints[0].path, "/api/ping");
        let value = serde_json::to_value(&endpoints[0]).unwrap();
        assert_eq!(value["httpMethod"], "GET");
        assert_eq!(value["controller"], "UserController");
    }
}
