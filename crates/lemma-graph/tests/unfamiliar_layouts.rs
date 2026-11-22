//! Checks on layouts first met outside Lemma's own systems: a YAML document
//! whose `servers` URL carries the prefix the routes are written under,
//! path parameters renamed, query parameters written into a path string,
//! a route removed beside another method's route on the same path, and gin
//! routers split by package. Each test commits a provider's base and head
//! and checks a TypeScript consumer against the change.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::fmt::Write as _;
use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, check};
use lemma_logic::ReferenceEngine;

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A document of the operations given as `(method, path, parameters)`,
/// served under `/api`.
fn spec(operations: &[(&str, &str, &str)]) -> String {
    let mut text =
        String::from("openapi: 3.0.0\nservers:\n  - url: https://example.test/api\npaths:\n");
    let mut paths: Vec<&str> = operations.iter().map(|(_, path, _)| *path).collect();
    paths.dedup();
    for path in paths {
        let _ = writeln!(text, "  {path}:");
        for (method, _, parameters) in operations.iter().filter(|operation| operation.1 == path) {
            let _ = writeln!(text, "    {method}:\n      parameters: [{parameters}]");
            text.push_str("      responses:\n        '200':\n          description: ok\n");
        }
    }
    text
}

struct Provider<'a> {
    base_spec: String,
    head_spec: String,
    /// Route code by file, at the base and at the head.
    base_code: &'a [(&'a str, &'a str)],
    head_code: &'a [(&'a str, &'a str)],
    include: &'a str,
}

/// Commits `provider`'s base and head as the repository `billing` under
/// `root`.
fn commit_provider(root: &Path, provider: &Provider<'_>) {
    let billing = root.join("billing");
    write(
        &billing.join("mod.lm"),
        &format!(
            "module billing\n\nimport * as http from lemma.http\n\nexport service api {{\n    index: {{ include: [{}] }}\n    operations: http.openapi(\"openapi.yml\")\n}}\n",
            provider.include
        ),
    );
    write(&billing.join("openapi.yml"), &provider.base_spec);
    for (file, text) in provider.base_code {
        write(&billing.join(file), text);
    }
    git(&billing, &["init", "-q"]);
    git(&billing, &["add", "."]);
    git(&billing, &["commit", "-q", "-m", "base"]);
    write(&billing.join("openapi.yml"), &provider.head_spec);
    for (file, _) in provider.base_code {
        std::fs::remove_file(billing.join(file)).unwrap();
    }
    for (file, text) in provider.head_code {
        write(&billing.join(file), text);
    }
    git(&billing, &["add", "-A"]);
    git(&billing, &["commit", "-q", "-m", "head"]);
}

/// The findings, as `requirement: link`, of checking `provider`'s change
/// against a consumer whose `src/api.ts` is `consumer`.
fn findings(provider: &Provider<'_>, consumer: &str) -> Vec<String> {
    let workspace = tempfile::tempdir().unwrap();
    let root = workspace.path();
    commit_provider(root, provider);
    let web = root.join("web");
    write(&web.join("mod.lm"), "module web\n\nexport service web {}\n");
    write(&web.join("src/api.ts"), consumer);
    git(&web, &["init", "-q"]);
    write(
        &root.join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n}\n",
    );
    let options = CheckOptions {
        change: Some(Change {
            repository: "#billing".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        level: Some(Level::Check),
        cache: root.join("cache"),
        cloning: lemma_graph::repos::Cloning::Allowed,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation: lemma_verify::Evaluation::Demand,
        list_links: false,
        run_checkers: true,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
        runtime: None,
    };
    let report = check(&root.join("mod.lm"), &options, &ReferenceEngine, None).unwrap();
    report
        .findings
        .iter()
        .filter(|finding| finding.introduced)
        .map(|finding| format!("{}: {}", finding.check, finding.link))
        .collect()
}

const PROFILE_CALLS: &str = r"export const follow = (name: string) => post(`/profiles/${name}/follow`);
export const unfollow = (name: string) => del(`/profiles/${name}/follow`);
export const feed = () => get('/articles/feed?limit=10&offset=0');
";

#[test]
fn an_operation_removed_from_a_yaml_document_is_reported() {
    let provider = Provider {
        base_spec: spec(&[
            ("get", "/articles/feed", ""),
            ("delete", "/profiles/{name}/follow", ""),
        ]),
        head_spec: spec(&[("get", "/articles/feed", "")]),
        base_code: &[],
        head_code: &[],
        include: "",
    };

    let found = findings(&provider, PROFILE_CALLS);

    assert!(
        found.iter().any(|finding| finding
            .starts_with("consumer_calls_removed_operation: #web:web:src/api.ts::unfollow -> DELETE /profiles/{}/follow")),
        "{found:#?}"
    );
}

#[test]
fn a_renamed_path_parameter_changes_nothing_a_caller_sends() {
    let parameter = |name: &str| {
        format!("{{name: {name}, in: path, required: true, schema: {{type: string}}}}")
    };
    let provider = Provider {
        base_spec: spec(&[(
            "post",
            "/profiles/{username}/follow",
            &parameter("username"),
        )]),
        head_spec: spec(&[("post", "/profiles/{name}/follow", &parameter("name"))]),
        base_code: &[],
        head_code: &[],
        include: "",
    };

    let found = findings(&provider, PROFILE_CALLS);

    assert_eq!(found, Vec::<String>::new());
}

#[test]
fn a_required_query_parameter_the_path_string_already_sends_is_no_finding() {
    let limit = "{name: limit, in: query, required: true, schema: {type: string}}";
    let provider = Provider {
        base_spec: spec(&[("get", "/articles/feed", "")]),
        head_spec: spec(&[("get", "/articles/feed", limit)]),
        base_code: &[],
        head_code: &[],
        include: "",
    };

    let found = findings(&provider, PROFILE_CALLS);

    assert_eq!(found, Vec::<String>::new());
}

const EXPRESS_BASE: &str = r"import { Router } from 'express';
const router = Router();
router.post('/profiles/:name/follow', follow);
router.delete('/profiles/:name/follow', unfollow);
export default Router().use('/api', router);
";

const EXPRESS_HEAD: &str = r"import { Router } from 'express';
const router = Router();
router.post('/profiles/:name/follow', follow);
export default Router().use('/api', router);
";

#[test]
fn a_route_removed_beside_another_methods_route_on_its_path_is_unrouted() {
    let both = [
        ("post", "/profiles/{name}/follow", ""),
        ("delete", "/profiles/{name}/follow", ""),
    ];
    let provider = Provider {
        base_spec: spec(&both),
        head_spec: spec(&both),
        base_code: &[("src/routes.ts", EXPRESS_BASE)],
        head_code: &[("src/routes.ts", EXPRESS_HEAD)],
        include: "\"src\"",
    };

    let found = findings(&provider, PROFILE_CALLS);

    assert!(
        found.iter().any(|finding| finding
            .starts_with("consumer_calls_unrouted_operation: #web:web:src/api.ts::unfollow -> DELETE /profiles/{}/follow")),
        "{found:#?}"
    );
}

const GIN_MAIN: &str = r#"package main

import "example.test/billing/profiles"

func main() {
	r := gin.Default()
	v1 := r.Group("/api")
	profiles.Register(v1.Group("/profiles"))
}
"#;

const GIN_PROFILES_BASE: &str = r#"package profiles

func Register(router *gin.RouterGroup) {
	router.POST("/:name/follow", Follow)
	router.DELETE("/:name/follow", Unfollow)
}
"#;

const GIN_PROFILES_HEAD: &str = r#"package profiles

func Register(router *gin.RouterGroup) {
	router.POST("/:name/follow", Follow)
}
"#;

#[test]
fn a_route_removed_from_a_gin_group_another_package_mounts_is_unrouted() {
    let both = [
        ("post", "/profiles/{name}/follow", ""),
        ("delete", "/profiles/{name}/follow", ""),
    ];
    let provider = Provider {
        base_spec: spec(&both),
        head_spec: spec(&both),
        base_code: &[
            ("main.go", GIN_MAIN),
            ("profiles/routes.go", GIN_PROFILES_BASE),
        ],
        head_code: &[
            ("main.go", GIN_MAIN),
            ("profiles/routes.go", GIN_PROFILES_HEAD),
        ],
        include: "\"main.go\", \"profiles\"",
    };

    let found = findings(&provider, PROFILE_CALLS);

    assert!(
        found.iter().any(|finding| finding
            .starts_with("consumer_calls_unrouted_operation: #web:web:src/api.ts::unfollow -> DELETE /profiles/{}/follow")),
        "{found:#?}"
    );
}
