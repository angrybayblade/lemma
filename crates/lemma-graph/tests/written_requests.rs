//! A name a request lacks is an error unless the request is passed in from
//! elsewhere and other code of the repository writes the name, and a
//! removed response field read one step off a call's result is read off
//! the body.

#![allow(clippy::unwrap_used, reason = "test setup should fail loudly")]

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Command;

use lemma_graph::config::Level;
use lemma_graph::{Change, CheckOptions, Severity, check};
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

fn commit(repository: &Path, subject: &str) {
    if !repository.join(".git").exists() {
        git(repository, &["init", "-q"]);
    }
    git(repository, &["add", "."]);
    git(repository, &["commit", "-q", "-m", subject]);
}

/// Billing's document. `POST /v1/users` takes `name` and a `profile` with
/// a `bio`, and requires what `required` and `required_in_profile` list;
/// `GET /v1/users` takes `limit`, and requires `parameters` too;
/// `GET /v1/invoices` returns `id`, `customer.email` and `returned`.
fn document(required: &str, required_in_profile: &str, parameters: &str, returned: &str) -> String {
    format!(
        r#"{{ "openapi": "3.0.0", "info": {{ "title": "B", "version": "1" }}, "paths": {{
  "/v1/users": {{
    "post": {{ "operationId": "createUser",
      "requestBody": {{ "content": {{ "application/json": {{ "schema": {{ "type": "object", "required": ["name", "profile"{required}],
        "properties": {{ "name": {{ "type": "string" }}, "email": {{ "type": "string" }},
          "profile": {{ "type": "object", "required": [{required_in_profile}],
            "properties": {{ "bio": {{ "type": "string" }}, "city": {{ "type": "string" }} }} }} }} }} }} }} }},
      "responses": {{ "200": {{ "description": "ok" }} }} }},
    "get": {{ "operationId": "listUsers",
      "parameters": [ {{ "name": "limit", "in": "query", "schema": {{ "type": "integer" }} }}{parameters} ],
      "responses": {{ "200": {{ "description": "ok" }} }} }} }},
  "/v1/invoices": {{
    "get": {{ "operationId": "getInvoice",
      "responses": {{ "200": {{ "description": "ok", "content": {{ "application/json": {{ "schema": {{ "type": "object",
        "properties": {{ "id": {{ "type": "string" }}, "customer": {{ "type": "object", "properties": {{ "email": {{ "type": "string" }} }} }}{returned} }} }} }} }} }} }} }} }} }} }}"#
    )
}

const AMOUNT_AND_STATUS: &str =
    r#", "amount": { "type": "number" }, "status": { "type": "string" }"#;
const TEAM_IN_QUERY: &str =
    r#", { "name": "team", "in": "query", "required": true, "schema": { "type": "string" } }"#;
const TRACE_IN_HEADER: &str =
    r#", { "name": "X-Trace", "in": "header", "required": true, "schema": { "type": "string" } }"#;

/// A workspace of billing, whose last commit changes its document from
/// `before` to `after`, and web, whose `src/calls.ts` is `calls`.
fn workspace(before: &str, after: &str, calls: &str) -> tempfile::TempDir {
    workspace_with(before, after, calls, "export const title = 'Shop'\n")
}

/// [`workspace`] with web's `src/form.ts`, a file nothing imports, being
/// `form`.
fn workspace_with(before: &str, after: &str, calls: &str, form: &str) -> tempfile::TempDir {
    let directory = tempfile::tempdir().unwrap();
    let billing = directory.path().join("billing");
    let web = directory.path().join("web");
    write(
        &billing.join("mod.lm"),
        "module billing\n\nimport * as http from lemma.http\n\nexport service api {\n    operations: http.openapi(\"openapi.json\")\n}\n",
    );
    write(&billing.join("openapi.json"), before);
    commit(&billing, "start billing");
    write(&billing.join("openapi.json"), after);
    commit(&billing, "change the contract");
    write(
        &web.join("mod.lm"),
        "module web\n\nexport service web {\n    index: { include: [\"src\"] }\n}\n",
    );
    write(&web.join("src/calls.ts"), calls);
    write(&web.join("src/form.ts"), form);
    commit(&web, "start web");
    write(
        &directory.path().join("mod.lm"),
        "module system\n\nworkspace {\n    repo billing = local(\"billing\")\n    repo web = local(\"web\")\n}\n",
    );
    directory
}

/// The findings of billing's last commit, as `(severity, requirement,
/// calling function)`, read whole and read change-driven.
fn found(workspace: &Path) -> Vec<(Severity, String, String)> {
    let asked = |evaluation| CheckOptions {
        change: Some(Change {
            repository: "billing".to_owned(),
            base: Some("HEAD~1".to_owned()),
            head: Some("HEAD".to_owned()),
        }),
        pins: BTreeMap::new(),
        level: Some(Level::Check),
        cache: workspace.join("cache"),
        cloning: lemma_graph::repos::Cloning::Allowed,
        explain: false,
        relations: Vec::new(),
        facts_cache: None,
        evaluation,
        list_links: false,
        run_checkers: false,
        runs_base_checkers: false,
        select: None,
        warm: None,
        environment: None,
        runtime: None,
    };
    let findings = |evaluation| -> Vec<(Severity, String, String)> {
        let report = check(
            &workspace.join("mod.lm"),
            &asked(evaluation),
            &ReferenceEngine,
            None,
        )
        .unwrap();
        report
            .findings
            .into_iter()
            .filter(|finding| finding.severity != Severity::Info)
            .map(|finding| {
                let function = finding
                    .message
                    .split_whitespace()
                    .next()
                    .and_then(|consumer| consumer.rsplit("::").next())
                    .unwrap_or_default()
                    .to_owned();
                (finding.severity, finding.check, function)
            })
            .collect()
    };
    let whole = findings(lemma_verify::Evaluation::Full);
    let change_driven = findings(lemma_verify::Evaluation::Demand);
    assert_eq!(whole, change_driven);
    whole
}

fn finding(severity: Severity, check: &str, function: &str) -> (Severity, String, String) {
    (severity, check.to_owned(), function.to_owned())
}

const PASSED: &str = "export async function passed(data: CreateUser) {\n  return axios.post('/v1/users', data)\n}\n\nexport async function enveloped(data: CreateUser) {\n  return request({ method: 'POST', url: '/v1/users', body: data })\n}\n\nexport async function spread(name: string, rest: Rest) {\n  return axios.post('/v1/users', { name, ...rest })\n}\n";

#[test]
fn a_new_required_field_is_an_error_where_the_request_is_written_at_the_call() {
    let calls = "export async function written(name: string, bio: string) {\n  return axios.post('/v1/users', { name, profile: { bio } })\n}\n";
    let shop = workspace_with(
        &document("", "", "", ""),
        &document(r#", "email""#, "", "", ""),
        calls,
        "export const draft = { email: '' }\n",
    );

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [finding(
            Severity::Error,
            "consumer_misses_new_required_field",
            "written"
        )]
    );
}

#[test]
fn a_new_required_field_is_a_warning_where_the_request_is_passed_in_and_other_code_names_it() {
    let shop = workspace_with(
        &document("", "", "", ""),
        &document(r#", "email""#, "", "", ""),
        PASSED,
        "export const draft = { email: '' }\n",
    );

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [
            finding(
                Severity::Warning,
                "consumer_may_miss_new_required_field",
                "enveloped"
            ),
            finding(
                Severity::Warning,
                "consumer_may_miss_new_required_field",
                "passed"
            ),
            finding(
                Severity::Warning,
                "consumer_may_miss_new_required_field",
                "spread"
            ),
        ]
    );
}

#[test]
fn a_new_required_field_no_file_of_the_repository_names_is_an_error_however_it_is_passed() {
    let shop = workspace(
        &document("", "", "", ""),
        &document(r#", "email""#, "", "", ""),
        PASSED,
    );

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [
            finding(
                Severity::Error,
                "consumer_misses_new_required_field",
                "enveloped"
            ),
            finding(
                Severity::Error,
                "consumer_misses_new_required_field",
                "passed"
            ),
            finding(
                Severity::Error,
                "consumer_misses_new_required_field",
                "spread"
            ),
        ]
    );
}

#[test]
fn a_field_required_under_one_given_a_value_from_elsewhere_is_a_warning() {
    let calls = "export async function passedProfile(name: string, profile: Profile) {\n  return axios.post('/v1/users', { name, profile })\n}\n\nexport async function writtenProfile(name: string, bio: string) {\n  return axios.post('/v1/users', { name, profile: { bio } })\n}\n";
    let shop = workspace_with(
        &document("", "", "", ""),
        &document("", r#""city""#, "", ""),
        calls,
        "export const draft = { city: '' }\n",
    );

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [
            finding(
                Severity::Error,
                "consumer_misses_new_required_field",
                "writtenProfile"
            ),
            finding(
                Severity::Warning,
                "consumer_may_miss_new_required_field",
                "passedProfile"
            ),
        ]
    );
}

const QUERIES: &str = "export async function written(limit: number) {\n  return axios.get('/v1/users', { params: { limit } })\n}\n\nexport async function passed(params: Query) {\n  return axios.get('/v1/users', { params })\n}\n\nexport async function built(filter: Filter) {\n  const query = new URLSearchParams(filter)\n  return axios.get(`/v1/users?${query}`)\n}\n";

#[test]
fn a_new_required_parameter_is_a_warning_where_the_query_is_passed_in_and_other_code_names_it() {
    let shop = workspace_with(
        &document("", "", "", ""),
        &document("", "", TEAM_IN_QUERY, ""),
        QUERIES,
        "export const filter = { team: 'core' }\n",
    );

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [
            finding(
                Severity::Error,
                "consumer_misses_new_required_parameter",
                "written"
            ),
            finding(
                Severity::Warning,
                "consumer_may_miss_new_required_parameter",
                "built"
            ),
            finding(
                Severity::Warning,
                "consumer_may_miss_new_required_parameter",
                "passed"
            ),
        ]
    );
}

#[test]
fn a_new_required_parameter_no_file_of_the_repository_names_is_an_error_however_it_is_passed() {
    let shop = workspace(
        &document("", "", "", ""),
        &document("", "", TEAM_IN_QUERY, ""),
        QUERIES,
    );

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [
            finding(
                Severity::Error,
                "consumer_misses_new_required_parameter",
                "built"
            ),
            finding(
                Severity::Error,
                "consumer_misses_new_required_parameter",
                "passed"
            ),
            finding(
                Severity::Error,
                "consumer_misses_new_required_parameter",
                "written"
            ),
        ]
    );
}

#[test]
fn a_parameter_set_by_a_call_on_the_query_is_sent() {
    let calls = "export async function set(team: string) {\n  const query = new URLSearchParams()\n  query.set('team', team)\n  return axios.get(`/v1/users?${query}`)\n}\n";
    let shop = workspace(
        &document("", "", "", ""),
        &document("", "", TEAM_IN_QUERY, ""),
        calls,
    );

    let findings = found(shop.path());

    assert_eq!(findings, []);
}

#[test]
fn a_new_required_header_is_a_warning_even_where_the_request_is_written_out() {
    let calls = "export async function written(limit: number) {\n  return axios.get('/v1/users', { params: { limit } })\n}\n";
    let shop = workspace(
        &document("", "", "", ""),
        &document("", "", TRACE_IN_HEADER, ""),
        calls,
    );

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [finding(
            Severity::Warning,
            "consumer_may_miss_new_required_header",
            "written"
        )]
    );
}

#[test]
fn a_removed_field_read_one_step_off_a_result_is_an_error_at_the_top_of_the_response() {
    let calls = "export async function amount(http: Http) {\n  const invoice = await http.get('/v1/invoices')\n  return invoice.amount\n}\n\nexport async function taken(http: Http) {\n  const { amount } = await http.get('/v1/invoices')\n  return amount\n}\n\nexport async function state(http: Http) {\n  const invoice = await http.get('/v1/invoices')\n  return invoice.status\n}\n";
    let shop = workspace(
        &document("", "", "", AMOUNT_AND_STATUS),
        &document("", "", "", ""),
        calls,
    );

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [
            finding(
                Severity::Error,
                "consumer_reads_removed_response_field",
                "amount"
            ),
            finding(
                Severity::Error,
                "consumer_reads_removed_response_field",
                "taken"
            ),
            finding(
                Severity::Warning,
                "consumer_uses_removed_response_field",
                "state"
            ),
        ]
    );
}

#[test]
fn a_name_from_deeper_in_the_response_read_one_step_off_a_result_stays_a_warning() {
    let calls = "export async function email(http: Http) {\n  const invoice = await http.get('/v1/invoices')\n  return invoice.email\n}\n";
    let before = document("", "", "", "");
    let after = before.replace(
        r#""customer": { "type": "object", "properties": { "email": { "type": "string" } } }"#,
        r#""customer": { "type": "object", "properties": { "name": { "type": "string" } } }"#,
    );
    let shop = workspace(&before, &after, calls);

    let findings = found(shop.path());

    assert_eq!(
        findings,
        [finding(
            Severity::Warning,
            "consumer_uses_removed_response_field",
            "email"
        )]
    );
}
