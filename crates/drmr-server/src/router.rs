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
    /// `GET /api/catalog`
    Catalog,
    /// `POST /api/convert`
    Convert,
    /// `GET /api/jobs/<job_id>`
    JobStatus,
    /// `POST /api/jobs/<job_id>/stop`
    JobStop,
    /// `GET /api/jobs/<job_id>/log`
    JobLog,
    /// `GET /api/jobs/<job_id>/log.jsonl`
    JobLogDownload,
    /// `GET /api/assets/<asset_id>`
    Assets,
    /// `GET /api/reports/catalog/<asset_id>`
    CatalogReport,
    /// `GET /reports/catalog/<asset_id>`
    CatalogReportPage,
    /// `GET /source/<asset_id>/<path:rel>`
    Source,
    /// `GET /dash/old`
    DashOld,
    /// `GET /output/<asset_id>/<path:rel>`
    Output,
    /// `GET|POST /clearkey/license`
    ClearkeyLicense,
    /// `POST /api/hls/analyses`
    HlsAnalyze,
    /// `GET /api/hls/analyses/<analysis_id>`
    HlsAnalysis,
    /// `POST /api/hls/packages`
    HlsPackage,
    /// `GET /api/hls/jobs`
    HlsJobs,
    /// `DELETE /api/hls/jobs/<job_id>`
    HlsJobDelete,
    /// `GET /api/hls/results/<result_id>`
    HlsResult,
    /// `GET /api/platform`
    Platform,
    /// `GET /api/queue`
    QueueState,
    /// `POST /api/queue`
    QueueAdd,
    /// `DELETE /api/queue/<asset_id>`
    QueueRemove,
    /// `GET /reports/hls/<result_id>`
    HlsReportPage,
    /// `POST /api/hls/playback-sessions`
    PlaybackSessions,
    /// `DELETE /api/hls/playback/<session_id>`
    PlaybackRevoke,
    /// `POST /api/hls/playback/<session_id>/license`
    PlaybackLicense,
    /// `GET /api/hls/playback/<session_id>/source/<resource_id>`
    PlaybackSource,
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
    Rule {
        parts: &[S("api"), S("catalog")],
        method: "GET",
        endpoint: Endpoint::Catalog,
    },
    Rule {
        parts: &[S("api"), S("convert")],
        method: "POST",
        endpoint: Endpoint::Convert,
    },
    Rule {
        parts: &[S("api"), S("jobs"), V],
        method: "GET",
        endpoint: Endpoint::JobStatus,
    },
    Rule {
        parts: &[S("api"), S("jobs"), V, S("stop")],
        method: "POST",
        endpoint: Endpoint::JobStop,
    },
    Rule {
        parts: &[S("api"), S("jobs"), V, S("log")],
        method: "GET",
        endpoint: Endpoint::JobLog,
    },
    Rule {
        parts: &[S("api"), S("jobs"), V, S("log.jsonl")],
        method: "GET",
        endpoint: Endpoint::JobLogDownload,
    },
    Rule {
        parts: &[S("api"), S("assets"), V],
        method: "GET",
        endpoint: Endpoint::Assets,
    },
    Rule {
        parts: &[S("api"), S("reports"), S("catalog"), V],
        method: "GET",
        endpoint: Endpoint::CatalogReport,
    },
    Rule {
        parts: &[S("reports"), S("catalog"), V],
        method: "GET",
        endpoint: Endpoint::CatalogReportPage,
    },
    Rule {
        parts: &[S("source"), V, P],
        method: "GET",
        endpoint: Endpoint::Source,
    },
    Rule {
        parts: &[S("dash"), S("old")],
        method: "GET",
        endpoint: Endpoint::DashOld,
    },
    Rule {
        parts: &[S("output"), V, P],
        method: "GET",
        endpoint: Endpoint::Output,
    },
    Rule {
        parts: &[S("clearkey"), S("license")],
        method: "GET",
        endpoint: Endpoint::ClearkeyLicense,
    },
    Rule {
        parts: &[S("clearkey"), S("license")],
        method: "POST",
        endpoint: Endpoint::ClearkeyLicense,
    },
    Rule {
        parts: &[S("api"), S("hls"), S("analyses")],
        method: "POST",
        endpoint: Endpoint::HlsAnalyze,
    },
    Rule {
        parts: &[S("api"), S("hls"), S("analyses"), V],
        method: "GET",
        endpoint: Endpoint::HlsAnalysis,
    },
    Rule {
        parts: &[S("api"), S("hls"), S("packages")],
        method: "POST",
        endpoint: Endpoint::HlsPackage,
    },
    Rule {
        parts: &[S("api"), S("hls"), S("jobs")],
        method: "GET",
        endpoint: Endpoint::HlsJobs,
    },
    Rule {
        parts: &[S("api"), S("hls"), S("jobs"), V],
        method: "DELETE",
        endpoint: Endpoint::HlsJobDelete,
    },
    Rule {
        parts: &[S("api"), S("hls"), S("results"), V],
        method: "GET",
        endpoint: Endpoint::HlsResult,
    },
    Rule {
        parts: &[S("api"), S("platform")],
        method: "GET",
        endpoint: Endpoint::Platform,
    },
    Rule {
        parts: &[S("api"), S("queue")],
        method: "GET",
        endpoint: Endpoint::QueueState,
    },
    Rule {
        parts: &[S("api"), S("queue")],
        method: "POST",
        endpoint: Endpoint::QueueAdd,
    },
    Rule {
        parts: &[S("api"), S("queue"), V],
        method: "DELETE",
        endpoint: Endpoint::QueueRemove,
    },
    Rule {
        parts: &[S("reports"), S("hls"), V],
        method: "GET",
        endpoint: Endpoint::HlsReportPage,
    },
    Rule {
        parts: &[S("api"), S("hls"), S("playback-sessions")],
        method: "POST",
        endpoint: Endpoint::PlaybackSessions,
    },
    Rule {
        parts: &[S("api"), S("hls"), S("playback"), V],
        method: "DELETE",
        endpoint: Endpoint::PlaybackRevoke,
    },
    Rule {
        parts: &[S("api"), S("hls"), S("playback"), V, S("license")],
        method: "POST",
        endpoint: Endpoint::PlaybackLicense,
    },
    Rule {
        parts: &[S("api"), S("hls"), S("playback"), V, S("source"), V],
        method: "GET",
        endpoint: Endpoint::PlaybackSource,
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
    fn werkzeug_matching() {
        assert_eq!(route("GET", "/"), Match::Found(Endpoint::Index, vec![]));
        assert_eq!(
            route("GET", "/output/o1/cbcs//v/init.mp4"),
            Match::Found(
                Endpoint::Output,
                vec!["o1".into(), "cbcs//v/init.mp4".into()]
            )
        );
        assert_eq!(
            route("GET", "/output/o1//cbcs/v/init.mp4"),
            Match::Redirect("/output/o1/cbcs/v/init.mp4".into())
        );
        assert_eq!(
            route("GET", "/api//catalog"),
            Match::Redirect("/api/catalog".into())
        );
        assert_eq!(
            route("GET", "//api/catalog"),
            Match::Found(Endpoint::Catalog, vec![])
        );
        assert_eq!(route("GET", "/api/catalog/"), Match::NotFound);
        assert_eq!(route("GET", "/api/jobs/a/b"), Match::NotFound);
        assert_eq!(
            route("PUT", "/clearkey/license"),
            Match::MethodNotAllowed(vec!["GET", "HEAD", "OPTIONS", "POST"])
        );
        assert_eq!(
            route("HEAD", "/api/convert"),
            Match::MethodNotAllowed(vec!["OPTIONS", "POST"])
        );
        assert_eq!(
            route("OPTIONS", "/api/queue"),
            Match::Options(vec!["GET", "HEAD", "OPTIONS", "POST"])
        );
        assert_eq!(
            route("HEAD", "/api/catalog"),
            Match::Found(Endpoint::Catalog, vec![])
        );
        assert_eq!(
            route("GET", "/api/hls/playback/s/source/media-x"),
            Match::Found(Endpoint::PlaybackSource, vec!["s".into(), "media-x".into()])
        );
    }
}
