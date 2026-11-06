use std::collections::BTreeMap;

use super::*;
use crate::load::{Repository, SourceError, Sources};
use crate::syntax::ast::Snapshot;
use crate::{Checked, check_program};

struct Memory(String);

impl Sources for Memory {
    fn read(&mut self, repository: &Repository, path: &str) -> Result<Option<String>, SourceError> {
        let is_entry = repository.source.is_none() && path == "lemma.lm";
        Ok(is_entry.then(|| self.0.clone()))
    }
}

fn checked(text: &str) -> Checked {
    let checked = check_program(&mut Memory(text.to_owned()), "lemma.lm");
    let errors = checked.error_lines();
    assert!(errors.is_empty(), "{errors:#?}");
    checked
}

/// Stated facts, and evidence answered from a table by argument text.
struct Answers<'m> {
    facts: StatedFacts<'m>,
    evidence: BTreeMap<String, Evidence>,
    demands: Vec<String>,
}

impl World for Answers<'_> {
    fn facts(&mut self, predicate: &crate::model::Predicate, snapshot: Snapshot) -> Facts {
        self.facts.facts(predicate, snapshot)
    }

    fn evidence(&mut self, demand: &Demand<'_>) -> Evidence {
        let key: Vec<String> = demand
            .arguments
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect();
        let key = key.join(" ");
        self.demands.push(key.clone());
        self.evidence
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Evidence::Unavailable("not in the table".to_owned()))
    }
}

fn run(checked: &Checked, evidence: &[(&str, Datum)]) -> (Vec<BehaviorResult>, Vec<String>) {
    let mut world = Answers {
        facts: StatedFacts::new(&checked.model, None),
        evidence: evidence
            .iter()
            .map(|(key, value)| {
                let observed = Evidence::Observed {
                    value: value.clone(),
                    key: (*key).to_owned(),
                    was_reused: false,
                };
                ((*key).to_owned(), observed)
            })
            .collect(),
        demands: Vec::new(),
    };
    let check = checked.model.checks.first();
    let results = run_check(&checked.model, check, &mut world);
    (results, world.demands)
}

fn object(fields: &[(&str, Datum)]) -> Datum {
    Datum::Record(
        fields
            .iter()
            .map(|(name, value)| ((*name).to_owned(), value.clone()))
            .collect(),
    )
}

/// A trace like the replay checker's: what the worker returned.
fn trace(retriable: Option<bool>) -> Datum {
    let mut body = vec![("error", Datum::Text("upstream failed".to_owned()))];
    if let Some(flag) = retriable {
        body.push(("retriable", Datum::Bool(flag)));
    }
    object(&[("status", Datum::Int(200)), ("body", object(&body))])
}

const WORKER: &str = r#"module platform

import { Link } from lemma.links

export service Worker {
    client operation call_setup_webhook {}
    export operation setup_trigger {}
}

export service Functions {
    export operation setup_webhook {}
}

export graph links : Link {
    Worker.call_setup_webhook -> Functions.setup_webhook
}

export checker replay(handler: Operation, through: Fact<Link>, upstream_retriable: Bool): Evidence<Json> {
    command: ["replay"]
}

export behavior forwards_retriable {
    let link = links[Worker.call_setup_webhook, Functions.setup_webhook]
    expect link.exists

    for flag in [true, false] {
        let trace = need replay(handler: Worker.setup_trigger, through: link, upstream_retriable: flag)
        expect trace.status == 200
        expect trace.body.retriable == flag
            because "the worker passes on the flag"
    }
}
"#;

const FORWARDS_TRUE: &str = "handler=Worker.setup_trigger through=links[Worker.call_setup_webhook, Functions.setup_webhook] upstream_retriable=true";
const FORWARDS_FALSE: &str = "handler=Worker.setup_trigger through=links[Worker.call_setup_webhook, Functions.setup_webhook] upstream_retriable=false";

#[test]
fn a_worker_that_forwards_the_flag_satisfies_the_behavior() {
    let checked = checked(WORKER);

    let (results, demands) = run(
        &checked,
        &[
            (FORWARDS_TRUE, trace(Some(true))),
            (FORWARDS_FALSE, trace(Some(false))),
        ],
    );

    assert_eq!(demands.len(), 2);
    assert_eq!(results[0].outcome, Outcome::Satisfied, "{results:#?}");
    assert_eq!(results[0].cases.len(), 3);
    assert_eq!(
        results[0].cases[1].identity,
        "platform::forwards_retriable[flag=true]"
    );
}

#[test]
fn a_worker_that_drops_the_flag_violates_it_saying_what_is_missing() {
    let checked = checked(WORKER);

    let (results, _) = run(
        &checked,
        &[(FORWARDS_TRUE, trace(None)), (FORWARDS_FALSE, trace(None))],
    );

    let result = &results[0];
    assert_eq!(result.outcome, Outcome::Violated);
    let problem = result.cases[1].problem.as_ref().unwrap();
    assert!(
        problem.message.contains("`retriable` is missing"),
        "{problem:#?}"
    );
    assert_eq!(
        problem.because.as_deref(),
        Some("the worker passes on the flag")
    );
}

#[test]
fn a_worker_that_flips_the_flag_reports_both_values() {
    let checked = checked(WORKER);

    let (results, _) = run(
        &checked,
        &[
            (FORWARDS_TRUE, trace(Some(false))),
            (FORWARDS_FALSE, trace(Some(false))),
        ],
    );

    let cases = &results[0].cases;
    assert_eq!(cases[1].outcome, Outcome::Violated);
    assert_eq!(cases[2].outcome, Outcome::Satisfied);
    let problem = cases[1].problem.as_ref().unwrap();
    assert_eq!(
        problem.operands,
        [
            ("trace.body.retriable".to_owned(), "false".to_owned()),
            ("flag".to_owned(), "true".to_owned())
        ]
    );
}

#[test]
fn a_checker_that_cannot_run_leaves_the_behavior_unknown() {
    let checked = checked(WORKER);

    let (results, _) = run(&checked, &[]);

    assert_eq!(results[0].outcome, Outcome::Unknown);
    assert!(
        results[0].cases[1]
            .problem
            .as_ref()
            .unwrap()
            .message
            .contains("unavailable")
    );
}

#[test]
fn deleting_the_link_violates_the_behavior_instead_of_looping_over_nothing() {
    let text = WORKER.replace(
        "    Worker.call_setup_webhook -> Functions.setup_webhook\n",
        "",
    );
    let checked = checked(&text);

    let (results, demands) = run(&checked, &[]);

    assert_eq!(results[0].outcome, Outcome::Violated);
    assert_eq!(results[0].cases.len(), 1, "the loop's cases never ran");
    assert_eq!(demands, Vec::<String>::new());
}

#[test]
fn a_behavior_that_asserts_nothing_is_unknown() {
    let checked = check_program(
        &mut Memory("module m\nbehavior empty {\n    let x = 1\n}\n".to_owned()),
        "lemma.lm",
    );

    let (results, _) = run(&checked, &[]);

    assert_eq!(results[0].outcome, Outcome::Unknown);
    assert_eq!(results[0].notes, ["no assertion ran"]);
}

#[test]
fn allow_empty_lets_a_behavior_pass_without_an_assertion() {
    let checked = checked("module m\nbehavior empty {\n    allow_empty: true\n    let x = 1\n}\n");

    let (results, _) = run(&checked, &[]);

    assert_eq!(results[0].outcome, Outcome::Satisfied);
}

#[test]
fn an_if_on_a_missing_field_ends_the_case_as_violated() {
    let text = r#"module m
checker probe(): Evidence<Json> {
    command: ["probe"]
}
behavior reads {
    let seen = need probe()
    if seen.flag {
        expect true
    }
}
"#;
    let checked = checked(text);

    let (results, _) = run(&checked, &[("", object(&[]))]);

    assert_eq!(results[0].outcome, Outcome::Violated);
    let problem = results[0].cases[0].problem.as_ref().unwrap();
    assert!(problem.message.contains("`flag`"), "{problem:#?}");
}

#[test]
fn false_and_anything_is_false_even_when_the_rest_is_missing() {
    let text = r#"module m
checker probe(): Evidence<Json> {
    command: ["probe"]
}
behavior reads {
    let seen = need probe()
    expect !(false && seen.absent)
    expect seen.has("present") && !seen.has("absent")
}
"#;
    let checked = checked(text);

    let (results, _) = run(&checked, &[("", object(&[("present", Datum::Null)]))]);

    assert_eq!(results[0].outcome, Outcome::Satisfied, "{results:#?}");
}

#[test]
fn functions_and_rules_are_evaluated() {
    let text = r#"module m
relation Owns(team: Text, service: Text)
graph owns : Owns {
    ("payments", "billing")
    ("payments", "ledger")
}
rule owned(service: Text) {
    (service) :- Owns(_, service)
}
fn twice(value: Int): Int {
    return value * 2
}
behavior counts {
    expect owned["ledger"].exists
    expect !owned["search"].exists
    expect twice(21) == 42
    for (team, service) in owns {
        expect team == "payments"
    }
}
"#;
    let checked = checked(text);

    let (results, _) = run(&checked, &[]);

    assert_eq!(results[0].outcome, Outcome::Satisfied, "{results:#?}");
    assert_eq!(results[0].cases.len(), 3);
}

#[test]
fn a_loop_over_facts_read_from_code_is_at_best_unknown() {
    let text = r"module m
import { calls } from lemma.http
behavior over_calls {
    allow_empty: true
    for (consumer, operation, file, line) in calls {
        expect line > 0
    }
}
";
    let checked = checked(text);

    let (results, _) = run(&checked, &[]);

    assert_eq!(results[0].outcome, Outcome::Unknown);
    assert!(results[0].notes[0].contains("come from code"));
}

#[test]
fn a_parameterised_behavior_runs_with_the_checks_arguments() {
    let text = r"module m
behavior at_least(limit: Int, value: Int) {
    expect value >= limit
}
check main {
    run at_least(limit: 3, value: 5)
    run at_least(limit: 9, value: 5) { severity: warning }
}
";
    let checked = checked(text);

    let (results, _) = run(&checked, &[]);

    assert_eq!(results[0].identity, "m::at_least(limit=3, value=5)");
    assert_eq!(results[0].outcome, Outcome::Satisfied);
    assert_eq!(results[1].outcome, Outcome::Violated);
    assert_eq!(results[1].severity, crate::model::Severity::Warning);
}

#[test]
fn a_function_that_calls_itself_is_reported() {
    let checked = check_program(
        &mut Memory("module m\nfn loop(value: Int): Int {\n    return loop(value)\n}\n".to_owned()),
        "lemma.lm",
    );

    assert!(
        checked
            .error_lines()
            .iter()
            .any(|line| line.contains("calls itself")),
        "{:#?}",
        checked.error_lines()
    );
}
