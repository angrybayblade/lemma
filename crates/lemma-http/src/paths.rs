//! HTTP paths in one canonical form, so a consumer's URL and an operation's
//! template can be compared.
//!
//! Parameters are written differently everywhere: `{id}` in `OpenAPI`,
//! `${id}` or `:id` in code, `[id]` in Next.js file names. Each becomes
//! `{}`. A scheme and host, a query string, text after the path, and a
//! trailing slash are dropped.
//!
//! Strings in code are paths only if they contain a *marker*: a leading
//! segment the services being checked actually serve, such as `/v1/`
//! ([`path_markers`]). Without one, every string starting with `/` would be
//! taken for a request.

/// The markers that make a string in code a path: `/<first segment>/` of
/// each operation path, sorted and unique. `/v1/users/{id}` gives `/v1/`.
pub fn path_markers<'a>(operation_paths: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut markers: Vec<String> = operation_paths
        .into_iter()
        .filter_map(|path| path.split('/').find(|segment| !segment.is_empty()))
        .filter(|segment| !segment.starts_with('{'))
        .map(|segment| format!("/{segment}/"))
        .collect();
    markers.sort();
    markers.dedup();
    markers
}

/// Where the first marker starts in `raw`, if any, counting only a marker
/// at the start of a URL's path: what comes before it is nothing, a scheme
/// and host, or a substitution such as `${BASE}` or `%s`. A marker inside
/// another path (`@/src/lib/internal/logger`, `kb/internal/`) is not one.
///
/// A marker's segment may also end the path (`https://shop.example/orders`
/// for `/orders/`), or be followed by a query or fragment.
fn marker_start(raw: &str, markers: &[String]) -> Option<usize> {
    markers
        .iter()
        .flat_map(|marker| {
            let segment = marker.trim_end_matches('/');
            raw.match_indices(segment)
                .filter(move |(at, _)| ends_segment(&raw[at + segment.len()..]))
                .map(|(at, _)| at)
        })
        .filter(|at| starts_url_path(&raw[..*at]))
        .min()
}

/// Whether `after` begins where a path segment ends: a slash, a query, a
/// fragment, the end of the path, or a character a path cannot hold.
fn ends_segment(after: &str) -> bool {
    after.chars().next().is_none_or(|next| {
        let is_path_character = next.is_alphanumeric() || matches!(next, '.' | '_' | '-' | '~');
        !is_path_character
    })
}

/// Whether `before` can precede a URL's path: it ends in something that
/// cannot be part of a path (a space, a quote), a substitution, or is a
/// scheme and host.
fn starts_url_path(before: &str) -> bool {
    let Some(last) = before.chars().next_back() else {
        return true;
    };
    if last == '}' || last == '$' || before.ends_with("%s") {
        return true;
    }
    let is_path_character =
        last.is_alphanumeric() || matches!(last, '.' | '_' | '-' | '/' | '@' | '~');
    if !is_path_character {
        return true;
    }
    let url_start = before
        .rfind(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '(' | '='))
        .map_or(before, |at| &before[at + 1..]);
    let Some((scheme, host)) = url_start.split_once("://") else {
        return false;
    };
    let is_scheme = !scheme.is_empty()
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '+');
    is_scheme && !host.is_empty() && !host.contains('/')
}

/// Whether `raw` contains one of the markers.
pub fn has_marker(raw: &str, markers: &[String]) -> bool {
    marker_start(raw, markers).is_some()
}

/// The canonical form of a path in a string that may or may not be one,
/// such as a consumer's literal: from its first marker on, so a base URL or
/// `${BASE}` before it is dropped. `None` without a marker.
pub fn canonical_path(raw: &str, markers: &[String]) -> Option<String> {
    let start = marker_start(raw, markers)?;
    Some(normalize(&raw[start..]))
}

/// The canonical form of a path known to be one, such as a spec's or a
/// route's: `/collections/{name}`, or a full URL. `None` if it is neither.
pub fn canonical_route(raw: &str) -> Option<String> {
    if let Some((_, after_scheme)) = raw.split_once("://") {
        let path = after_scheme
            .find('/')
            .map_or("/", |start| &after_scheme[start..]);
        return Some(normalize(path));
    }
    raw.starts_with('/').then(|| normalize(raw))
}

/// Parameters as `{}`, without a query, a fragment, text after the path or
/// a trailing slash.
fn normalize(path: &str) -> String {
    let path = path
        .split(|c: char| c == '?' || c == '#' || c.is_whitespace())
        .next()
        .unwrap_or(path);
    let segments: Vec<&str> = path
        .split('/')
        .skip(1)
        .filter(|segment| !segment.is_empty())
        .map(|segment| if is_parameter(segment) { "{}" } else { segment })
        .collect();
    format!("/{}", segments.join("/"))
}

/// An HTTP method in upper case, or `*` when unknown.
pub fn canonical_method(raw: &str) -> String {
    let upper = raw.trim().to_ascii_uppercase();
    let known = ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"];
    if known.contains(&upper.as_str()) {
        upper
    } else {
        "*".to_owned()
    }
}

/// The method written just before a path, as in `PATCH /v1/users/{id}` in
/// documentation or a log line, or `None` if the word before it is not one.
pub fn method_before_path(raw: &str, markers: &[String]) -> Option<String> {
    let start = marker_start(raw, markers)?;
    let before = raw[..start].trim_end_matches(|c: char| !c.is_whitespace());
    let word = before.split_whitespace().next_back()?;
    let method = canonical_method(word);
    (method != "*").then_some(method)
}

/// `{id}`, `${id}`, `:id`, `[id]`, a `%s` format verb, or a segment that is
/// wholly a template substitution. A segment mixing text and a parameter
/// (`v${n}`) counts too.
fn is_parameter(segment: &str) -> bool {
    segment.starts_with('{')
        || segment.starts_with(':')
        || segment.starts_with('[')
        || segment.starts_with('%')
        || segment.contains("${")
}

/// Whether a file is a test, by the naming conventions of the languages
/// Lemma reads: tests are not part of what a service serves or a consumer
/// calls.
pub fn is_test_file(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.ends_with("_test.go")
        || lower.ends_with("_test.py")
        || lower.contains("/test/")
        || lower.contains("/tests/")
        || lower.contains("__tests__")
        || lower.contains(".test.")
        || lower.contains(".spec.")
        || lower.contains("/test_")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn markers() -> Vec<String> {
        path_markers(["/v1/users/{id}", "/v1/orders", "/health"])
    }

    #[test]
    fn markers_are_the_leading_segments_services_serve() {
        assert_eq!(markers(), ["/health/", "/v1/"]);
    }

    #[test]
    fn a_marker_inside_another_path_is_not_a_request() {
        let markers = vec!["/internal/".to_owned(), "/v1/".to_owned()];

        assert_eq!(
            canonical_path("@/src/common/lib/internal/logger", &markers),
            None
        );
        assert_eq!(canonical_path("kb/internal/", &markers), None);
        assert_eq!(
            canonical_path("https://api.example.com/v1/users", &markers).as_deref(),
            Some("/v1/users")
        );
        assert_eq!(
            canonical_path("{THERMOS_BASE_URL}/internal/x", &markers).as_deref(),
            Some("/internal/x")
        );
    }

    #[test]
    fn strings_are_paths_only_with_a_marker() {
        assert_eq!(
            canonical_path("${BASE_URL}/v1/users/${id}", &markers()).as_deref(),
            Some("/v1/users/{}")
        );
        assert_eq!(canonical_path("/static/logo.svg", &markers()), None);
        assert_eq!(canonical_path("/v1/users", &[]), None);
    }

    #[test]
    fn routes_and_urls_need_no_marker() {
        assert_eq!(
            canonical_route("/collections/{collection_name}/points").as_deref(),
            Some("/collections/{}/points")
        );
        assert_eq!(
            canonical_route("https://api.example.com/v1/users/:id/").as_deref(),
            Some("/v1/users/{}")
        );
        assert_eq!(canonical_route("collections"), None);
    }

    #[test]
    fn a_method_written_before_a_path_is_found() {
        let documented = "PATCH /v1/users/{user_id}\n\n{ \"x\": 1 }";
        let full_url = "GET https://api.example.com/v1/users";

        assert_eq!(
            method_before_path(documented, &markers()).as_deref(),
            Some("PATCH")
        );
        assert_eq!(
            method_before_path(full_url, &markers()).as_deref(),
            Some("GET")
        );
        assert_eq!(method_before_path("${BASE}/v1/users", &markers()), None);
        assert_eq!(
            canonical_path(documented, &markers()).as_deref(),
            Some("/v1/users/{}")
        );
    }

    #[test]
    fn parameter_styles_become_one_placeholder() {
        let forms = [
            "/v1/users/{user_id}",
            "${BASE}/v1/users/${id}",
            "https://api.example.com/v1/users/:id/",
            "/v1/users/[userId]?expand=true",
            "%s/v1/users/%s",
        ];

        let canonical: Vec<_> = forms
            .iter()
            .map(|form| canonical_path(form, &markers()))
            .collect();

        assert!(
            canonical
                .iter()
                .all(|path| path.as_deref() == Some("/v1/users/{}"))
        );
    }
}
