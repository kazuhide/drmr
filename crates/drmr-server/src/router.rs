//! HTTP routes, method matching and normalized path parameters.

/// Every endpoint of the application.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Endpoint {
    /// The common CLI console resources.
    Common,
    /// `GET /`
    Index,
    /// `GET /static/<path:filename>`
    Static,
}

#[derive(Clone, Copy)]
enum Part {
    Static(&'static str),
    Var,
    Path,
}

struct Rule {
    parts: &'static [Part],
    method: &'static str,
    endpoint: Endpoint,
}

use Part::{Path as P, Static as S, Var as V};

const RULES: &[Rule] = &[
    Rule {
        parts: &[S("api"), S("common"), S("capabilities")],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("files")],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("sessions")],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("sessions")],
        method: "POST",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("session-requests"), V],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("sessions"), V],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("sessions"), V],
        method: "DELETE",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("jobs")],
        method: "POST",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("sessions"), V, S("requests"), V],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("jobs"), V],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("jobs"), V, S("stop")],
        method: "POST",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("analyses"), V],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("plans"), V],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("plans"), V, S("export")],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("api"), S("common"), S("reports"), V],
        method: "GET",
        endpoint: Endpoint::Common,
    },
    Rule {
        parts: &[S("")],
        method: "GET",
        endpoint: Endpoint::Index,
    },
    Rule {
        parts: &[S("static"), P],
        method: "GET",
        endpoint: Endpoint::Static,
    },
];

/// The outcome of matching a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Match {
    /// The endpoint and its variables, in order.
    Found(Endpoint, Vec<String>),
    /// `OPTIONS` on a known path: the allowed methods.
    Options(Vec<&'static str>),
    /// The path matches, the method does not: the allowed methods.
    MethodNotAllowed(Vec<&'static str>),
    /// The path matches once its consecutive slashes are merged (the merged path).
    Redirect(String),
    /// No rule matches.
    NotFound,
}

/// The methods a rule allows: its own, `HEAD` with `GET`, and `OPTIONS`.
fn rule_methods(method: &'static str) -> Vec<&'static str> {
    if method == "GET" {
        vec!["GET", "HEAD", "OPTIONS"]
    } else {
        vec![method, "OPTIONS"]
    }
}

fn match_parts(parts: &[Part], segments: &[&str]) -> Option<Vec<String>> {
    let mut values = Vec::new();
    let mut index = 0;
    for (position, part) in parts.iter().enumerate() {
        match part {
            Part::Static(text) => {
                if segments.get(index) != Some(text) {
                    return None;
                }
                index += 1;
            }
            Part::Var => {
                let segment = segments.get(index)?;
                if segment.is_empty() || segment.contains('\n') {
                    return None;
                }
                values.push((*segment).to_string());
                index += 1;
            }
            Part::Path => {
                debug_assert_eq!(position, parts.len() - 1, "a path variable is last");
                let rest = segments.get(index..)?.join("/");
                if rest.is_empty() || rest.starts_with('/') || rest.contains('\n') {
                    return None;
                }
                values.push(rest);
                index = segments.len();
            }
        }
    }
    (index == segments.len()).then_some(values)
}

/// The rules matching `path` (one leading slash), with their variables.
fn candidates(path: &str) -> Vec<(&'static Rule, Vec<String>)> {
    let trimmed = path.strip_prefix('/').unwrap_or(path);
    let segments: Vec<&str> = trimmed.split('/').collect();
    RULES
        .iter()
        .filter_map(|rule| match_parts(rule.parts, &segments).map(|values| (rule, values)))
        .collect()
}

fn sorted_methods(rules: &[(&'static Rule, Vec<String>)]) -> Vec<&'static str> {
    let mut methods: Vec<&'static str> = rules
        .iter()
        .flat_map(|(rule, _)| rule_methods(rule.method))
        .collect();
    methods.sort_unstable();
    methods.dedup();
    methods
}

/// Match a request's decoded path and method.
pub fn route(method: &str, path: &str) -> Match {
    let path = format!("/{}", path.trim_start_matches('/'));
    let found = candidates(&path);
    if !found.is_empty() {
        return dispatch(method, &found);
    }
    if path.contains("//") {
        let mut merged = String::with_capacity(path.len());
        for c in path.chars() {
            if c == '/' && merged.ends_with('/') {
                continue;
            }
            merged.push(c);
        }
        let found = candidates(&merged);
        if !found.is_empty() {
            return match dispatch(method, &found) {
                Match::Found(..) | Match::Options(_) => Match::Redirect(merged),
                other => other,
            };
        }
    }
    Match::NotFound
}

fn dispatch(method: &str, found: &[(&'static Rule, Vec<String>)]) -> Match {
    let lookup = if method == "HEAD" { "GET" } else { method };
    if let Some((rule, values)) = found.iter().find(|(rule, _)| rule.method == lookup) {
        return Match::Found(rule.endpoint, values.clone());
    }
    if method == "OPTIONS" {
        return Match::Options(sorted_methods(found));
    }
    Match::MethodNotAllowed(sorted_methods(found))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn common_and_static_routes_keep_method_and_path_boundaries() {
        assert_eq!(route("GET", "/"), Match::Found(Endpoint::Index, vec![]));
        assert_eq!(
            route("GET", "/api//common/capabilities"),
            Match::Redirect("/api/common/capabilities".into())
        );
        assert_eq!(
            route("GET", "//api/common/capabilities"),
            Match::Found(Endpoint::Common, vec![])
        );
        assert_eq!(route("GET", "/api/common/capabilities/"), Match::NotFound);
        assert_eq!(route("GET", "/api/common/jobs/a/b"), Match::NotFound);
        assert_eq!(
            route("PUT", "/api/common/sessions"),
            Match::MethodNotAllowed(vec!["GET", "HEAD", "OPTIONS", "POST"])
        );
        assert_eq!(
            route("HEAD", "/api/common/jobs"),
            Match::MethodNotAllowed(vec!["OPTIONS", "POST"])
        );
        assert_eq!(
            route("OPTIONS", "/api/common/sessions"),
            Match::Options(vec!["GET", "HEAD", "OPTIONS", "POST"])
        );
        assert_eq!(
            route("HEAD", "/api/common/capabilities"),
            Match::Found(Endpoint::Common, vec![])
        );
        assert_eq!(
            route("GET", "/static/js/common-view.js"),
            Match::Found(Endpoint::Static, vec!["js/common-view.js".into()])
        );
    }
    #[test]
    fn retired_routes_are_not_executable_or_readable() {
        for path in [
            "/api/catalog",
            "/api/convert",
            "/api/jobs/a",
            "/api/hls/analyses",
            "/api/hls/packages",
            "/api/platform",
            "/api/queue",
            "/reports/catalog/a",
            "/reports/hls/a",
            "/source/a/file",
            "/output/a/file",
            "/dash/old",
            "/clearkey/license",
        ] {
            for method in ["GET", "POST", "DELETE", "OPTIONS"] {
                assert_eq!(route(method, path), Match::NotFound, "{method} {path}");
            }
        }
    }
}
