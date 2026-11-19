//! OpenAPI 3 JSON for the scanned Spring endpoints.

use crate::parser::Endpoint;
use serde_json::{Map, Value, json};

pub fn document(endpoints: &[Endpoint], title: &str) -> Value {
    let mut paths: Map<String, Value> = Map::new();
    for endpoint in endpoints {
        let method = endpoint.http_method.to_ascii_lowercase();
        let mut parameters = Vec::new();
        for name in crate::request::path_params(&endpoint.path) {
            parameters.push(json!({
                "name": name,
                "in": "path",
                "required": true,
                "schema": { "type": "string" },
            }));
        }
        for parameter in &endpoint.query {
            let mut schema = json!({ "type": "string" });
            if let Some(value) = &parameter.value {
                schema["default"] = json!(value);
            }
            parameters.push(json!({
                "name": parameter.name,
                "in": "query",
                "required": false,
                "schema": schema,
            }));
        }
        let mut operation = json!({
            "operationId": format!("{}_{}", endpoint.controller, endpoint.method_name),
            "summary": endpoint.method_name,
            "parameters": parameters,
            "responses": { "200": { "description": "OK" } },
        });
        if let Some(_body_type) = &endpoint.body_type {
            let content_type = endpoint
                .consumes
                .clone()
                .unwrap_or_else(|| "application/json".into());
            operation["requestBody"] = json!({
                "content": { content_type: { "schema": { "type": "object" } } },
            });
        }
        let path = paths
            .entry(endpoint.path.clone())
            .or_insert_with(|| json!({}));
        if let Value::Object(operations) = path {
            operations.insert(method, operation);
        }
    }
    json!({
        "openapi": "3.0.3",
        "info": { "title": title, "version": "1.0.0" },
        "paths": paths,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_source;

    #[test]
    fn writes_paths_parameters_and_request_bodies() {
        let endpoints = parse_source(
            "/p/UserController.java",
            r#"
@RestController
@RequestMapping("/api/users")
public class UserController {
    @GetMapping("/{id}")
    public User get(@PathVariable Long id, @RequestParam(name = "q", defaultValue = "all") String q) { return null; }
    @PostMapping(value = "/", consumes = "application/json")
    public User create(@RequestBody User body) { return null; }
}
"#,
        );
        let document = document(&endpoints, "foodie");
        assert_eq!(document["openapi"], "3.0.3");
        assert_eq!(document["info"]["title"], "foodie");
        let get = &document["paths"]["/api/users/{id}"]["get"];
        assert_eq!(get["operationId"], "UserController_get");
        let parameters = get["parameters"].as_array().unwrap();
        assert_eq!(parameters[0]["in"], "path");
        assert_eq!(parameters[0]["name"], "id");
        assert_eq!(parameters[1]["in"], "query");
        assert_eq!(parameters[1]["schema"]["default"], "all");
        let post = &document["paths"]["/api/users"]["post"];
        assert_eq!(
            post["requestBody"]["content"]["application/json"]["schema"]["type"],
            "object"
        );
    }
}
