//! HTTP paths in one canonical form, so a consumer's URL and an operation's
//! template can be compared.
//!
//! Parameters are written differently everywhere: `{nanoid}` in `OpenAPI`,
//! `${id}` or `:id` in code, `[nanoId]` in Next.js file names. Each becomes
//! `{}`. Anything before `/api/` (a base URL, `${BACKEND}`), a query
//! string, and a trailing slash are dropped.

/// The canonical form of a path, or `None` if it has no `/api/` part.
pub fn canonical_path(raw: &str) -> Option<String> {
    let start = raw.find("/api/")?;
    let path = &raw[start..];
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let segments: Vec<&str> = path
        .split('/')
        .skip(1)
        .filter(|segment| !segment.is_empty())
        .map(|segment| if is_parameter(segment) { "{}" } else { segment })
        .collect();
    Some(format!("/{}", segments.join("/")))
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

/// `{id}`, `${id}`, `:id`, `[id]` or a segment that is wholly a template
/// substitution. A segment mixing text and a parameter (`v${n}`) counts too.
fn is_parameter(segment: &str) -> bool {
    segment.starts_with('{')
        || segment.starts_with(':')
        || segment.starts_with('[')
        || segment.contains("${")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parameter_styles_become_one_placeholder() {
        let forms = [
            "/api/v3.1/connected_accounts/{nanoid}",
            "${BACKEND}/api/v3.1/connected_accounts/${id}",
            "https://backend.sdk.dev/api/v3.1/connected_accounts/:id/",
            "/api/v3.1/connected_accounts/[nanoId]?expand=true",
        ];

        let canonical: Vec<_> = forms.iter().map(|form| canonical_path(form)).collect();

        assert!(
            canonical
                .iter()
                .all(|path| path.as_deref() == Some("/api/v3.1/connected_accounts/{}"))
        );
    }

    #[test]
    fn strings_without_an_api_path_are_ignored() {
        assert_eq!(canonical_path("https://sdk.dev/docs"), None);
    }

    #[test]
    fn methods_are_upper_case_or_unknown() {
        assert_eq!(canonical_method("post"), "POST");
        assert_eq!(canonical_method("fetch"), "*");
    }
}
