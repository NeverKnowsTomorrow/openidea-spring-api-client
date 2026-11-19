//! Static Spring Cloud Gateway route discovery from Java/Kotlin route DSL and
//! `application.yml`. Runtime `RouteLocator` beans still need a JVM agent.

use openidea_plugin::Project;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Route {
    pub id: String,
    pub uri: String,
    pub path: Option<String>,
    pub strip_prefix: Option<u32>,
}

pub fn discover(project: &Project) -> Vec<Route> {
    let mut routes = Vec::new();
    for pattern in ["**/*.java", "**/*.kt"] {
        if let Ok(files) = project.find_files(pattern, 300) {
            for path in files {
                if let Ok(text) = project.read_file(&path) {
                    routes.extend(parse_dsl(&text));
                }
            }
        }
    }
    for pattern in ["**/application.yml", "**/application.yaml"] {
        if let Ok(files) = project.find_files(pattern, 100) {
            for path in files {
                if let Ok(text) = project.read_file(&path) {
                    routes.extend(parse_yaml(&text));
                }
            }
        }
    }
    routes.retain(|route| route.uri.starts_with("http://") || route.uri.starts_with("https://"));
    routes.sort_by(|a, b| a.id.cmp(&b.id));
    routes.dedup_by(|a, b| a.id == b.id);
    routes
}

pub fn parse_dsl(text: &str) -> Vec<Route> {
    let mut routes = Vec::new();
    let mut from = 0;
    while let Some(at) = text[from..].find(".route(").map(|at| from + at) {
        from = at + 1;
        let slice = &text[at..text.len().min(at + 1200)];
        let id = string_after(slice, "route(").unwrap_or_default();
        if id.is_empty() {
            continue;
        }
        let Some(uri) = string_after(slice, ".uri(") else {
            continue;
        };
        let strip_prefix = number_after(slice, ".stripPrefix(");
        routes.push(Route {
            id,
            uri,
            path: string_after(slice, ".path("),
            strip_prefix,
        });
    }
    routes
}

pub fn parse_yaml(text: &str) -> Vec<Route> {
    let mut routes = Vec::new();
    let mut in_routes = false;
    let mut routes_indent = 0usize;
    let mut current: Option<Route> = None;
    let flush = |routes: &mut Vec<Route>, current: &mut Option<Route>| {
        if let Some(route) = current.take()
            && !route.id.is_empty()
            && !route.uri.is_empty()
        {
            routes.push(route);
        }
    };
    for raw in text.lines() {
        let line = raw.split('#').next().unwrap_or(raw).trim_end();
        if line.trim().is_empty() {
            continue;
        }
        let indent = line.len() - line.trim_start().len();
        let trimmed = line.trim();
        if !in_routes {
            if trimmed == "routes:" {
                in_routes = true;
                routes_indent = indent;
            }
            continue;
        }
        if indent <= routes_indent {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("- id:") {
            flush(&mut routes, &mut current);
            current = Some(Route {
                id: rest.trim().trim_matches(['"', '\'']).to_owned(),
                ..Route::default()
            });
        } else if let Some(rest) = trimmed.strip_prefix("id:") {
            if current.is_none() {
                current = Some(Route::default());
            }
            if let Some(route) = &mut current {
                route.id = rest.trim().trim_matches(['"', '\'']).to_owned();
            }
        } else if let Some(rest) = trimmed.strip_prefix("uri:") {
            if let Some(route) = &mut current {
                route.uri = rest.trim().trim_matches(['"', '\'']).to_owned();
            }
        } else if let Some(rest) = trimmed.strip_prefix("- Path=") {
            if let Some(route) = &mut current {
                route.path = Some(rest.trim().to_owned());
            }
        } else if let Some(rest) = trimmed.strip_prefix("- StripPrefix=") {
            if let Some(route) = &mut current {
                route.strip_prefix = rest.trim().parse().ok();
            }
        }
    }
    flush(&mut routes, &mut current);
    routes
}

fn string_after(text: &str, marker: &str) -> Option<String> {
    let at = text.find(marker)? + marker.len();
    let rest = &text[at..];
    let quote_at = rest.find(['"', '\''])?;
    let quote = rest.as_bytes()[quote_at] as char;
    let rest = &rest[quote_at + 1..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_owned())
}

fn number_after(text: &str, marker: &str) -> Option<u32> {
    let at = text.find(marker)? + marker.len();
    let digits: String = text[at..]
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_java_route_dsl() {
        let routes = parse_dsl(
            r#"
@Bean
RouteLocator routes(RouteLocatorBuilder builder) {
    return builder.routes()
        .route("apiroute", r -> r.path("/api/**")
            .filters(f -> f.stripPrefix(1))
            .uri("http://localhost:8080"))
        .build();
}
"#,
        );
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].id, "apiroute");
        assert_eq!(routes[0].uri, "http://localhost:8080");
        assert_eq!(routes[0].path.as_deref(), Some("/api/**"));
        assert_eq!(routes[0].strip_prefix, Some(1));
    }

    #[test]
    fn reads_yaml_gateway_routes() {
        let routes = parse_yaml(
            r#"
spring:
  cloud:
    gateway:
      routes:
        - id: apiroute
          uri: http://localhost:8080
          predicates:
            - Path=/api/**
          filters:
            - StripPrefix=1
"#,
        );
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].id, "apiroute");
        assert_eq!(routes[0].uri, "http://localhost:8080");
        assert_eq!(routes[0].path.as_deref(), Some("/api/**"));
    }
}
