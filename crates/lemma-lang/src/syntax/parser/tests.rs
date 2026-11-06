use super::*;
use crate::syntax::ast::{
    AtomArguments, CheckMember, Declaration, ExpressionKind, GraphBody, ImportSelection, Item,
    Literal, ServiceMember, Snapshot, Statement, TermKind,
};

fn parsed(source: &str) -> File {
    let parsed = parse(source);
    assert!(
        parsed.diagnostics.is_empty(),
        "unexpected diagnostics: {:#?}",
        parsed.diagnostics
    );
    parsed.file
}

fn declarations(file: &File) -> Vec<&Declaration> {
    file.items
        .iter()
        .filter_map(|item| match item {
            Item::Declaration(declared) => Some(&declared.declaration),
            _ => None,
        })
        .collect()
}

#[test]
fn a_module_line_and_imports_are_read() {
    let file = parsed(
        "module platform.services.worker\n\nimport { Link, Other as Renamed } from lemma.links\nimport * as go from lemma.go\nimport platform.services.gateway\n",
    );

    assert_eq!(
        file.module.map(|name| name.dotted()).as_deref(),
        Some("platform.services.worker")
    );
    let Item::Import(first) = &file.items[0] else {
        panic!("expected an import");
    };
    let ImportSelection::Names(names) = &first.selection else {
        panic!("expected named imports");
    };
    assert_eq!(names[1].local().text, "Renamed");
    let Item::Import(third) = &file.items[2] else {
        panic!("expected an import");
    };
    assert_eq!(third.selection, ImportSelection::Module);
}

#[test]
fn a_service_holds_fields_operations_and_nested_services() {
    let file = parsed(
        "module functions\nexport service Functions {\n    index: { include: [\"functions\"] }\n    export service serverless {\n        export operation setup_webhook {\n            dispatch: \"setup_webhook\"\n        }\n    }\n    client operation call { sends: t }\n}\n",
    );

    let Declaration::Service(service) = declarations(&file)[0] else {
        panic!("expected a service");
    };
    assert!(
        matches!(&service.members[0], ServiceMember::Field(field) if field.name.text == "index")
    );
    assert!(matches!(
        &service.members[1],
        ServiceMember::Service {
            is_exported: true,
            ..
        }
    ));
    assert!(matches!(
        &service.members[2],
        ServiceMember::Operation { operation, .. } if operation.is_client
    ));
}

#[test]
fn rule_clauses_read_variables_constants_snapshots_and_negation() {
    let file = parsed(
        "module m\nrule removed(target: Text) {\n    (target) :-\n        operation@base(operation: target),\n        !operation@head(operation: target),\n        target != \"*\"\n}\n",
    );

    let Declaration::Rule(rule) = declarations(&file)[0] else {
        panic!("expected a rule");
    };
    let literals = &rule.clauses[0].body.literals;
    let Literal::Atom(base) = &literals[0] else {
        panic!("expected an atom");
    };
    assert_eq!(base.snapshot.map(Snapshot::as_str), Some("base"));
    assert!(
        matches!(&base.arguments, AtomArguments::Named(named) if named[0].name.text == "operation")
    );
    assert!(matches!(&literals[1], Literal::Atom(atom) if atom.is_negated));
    assert!(matches!(&literals[2], Literal::Comparison(_)));
}

#[test]
fn spelling_decides_variables_and_constants() {
    let file = parsed(
        "module m\nrule r(a: Text) {\n    (a) :- graph(a, Worker.call, self.production, _, consumer.file)\n}\n",
    );

    let Declaration::Rule(rule) = declarations(&file)[0] else {
        panic!("expected a rule");
    };
    let Literal::Atom(atom) = &rule.clauses[0].body.literals[0] else {
        panic!("expected an atom");
    };
    let AtomArguments::Positional(terms) = &atom.arguments else {
        panic!("expected positional arguments");
    };
    assert!(matches!(&terms[0].kind, TermKind::Variable(name) if name == "a"));
    assert!(matches!(&terms[1].kind, TermKind::Constant(path) if path.root.is_some()));
    assert!(matches!(&terms[2].kind, TermKind::Constant(path) if path.root.is_none()));
    assert!(matches!(&terms[3].kind, TermKind::Wildcard));
    assert!(matches!(&terms[4].kind, TermKind::Field { .. }));
}

#[test]
fn a_requirement_reads_its_when_as_a_body() {
    let file = parsed(
        "module m\nexport require broken {\n    severity: error\n    when:\n        calls(consumer, operation, file, line),\n        removed(operation)\n    message: \"{consumer} calls {operation}\"\n}\n",
    );

    let Declaration::Require(require) = declarations(&file)[0] else {
        panic!("expected a requirement");
    };
    assert_eq!(
        require.when.as_ref().map(|body| body.literals.len()),
        Some(2)
    );
    assert_eq!(require.fields.len(), 2);
}

#[test]
fn graph_rows_take_arrows_tuples_and_single_values() {
    let file = parsed(
        "module m\ngraph links : Link {\n    Worker.call -> Functions.serverless.setup\n    (Gateway, \"/x\", Worker.trigger)\n    production\n}\ngraph found : Link = http.inferred_links(services: [Worker])\n",
    );

    let found = declarations(&file);
    let Declaration::Graph(rows) = found[0] else {
        panic!("expected a graph");
    };
    let GraphBody::Rows(rows) = &rows.body else {
        panic!("expected rows");
    };
    let widths: Vec<usize> = rows.iter().map(|row| row.values.len()).collect();
    assert_eq!(widths, [2, 3, 1]);
    assert!(
        matches!(found[1], Declaration::Graph(graph) if matches!(graph.body, GraphBody::Provider(_)))
    );
}

#[test]
fn a_line_continues_after_an_operator_and_before_because() {
    let file = parsed(
        "module m\nbehavior b {\n    expect a ==\n        b\n        because \"reason\"\n    expect c\n}\n",
    );

    let Declaration::Behavior(behavior) = declarations(&file)[0] else {
        panic!("expected a behavior");
    };
    assert_eq!(behavior.body.statements.len(), 2);
    assert!(matches!(
        &behavior.body.statements[0],
        Statement::Expect {
            because: Some(_),
            ..
        }
    ));
}

#[test]
fn else_may_start_the_line_after_the_block() {
    let file = parsed(
        "module m\nfn f(a: Bool): Int {\n    if a {\n        return 1\n    }\n    else {\n        return 2\n    }\n}\n",
    );

    let Declaration::Function(function) = declarations(&file)[0] else {
        panic!("expected a function");
    };
    assert!(
        matches!(&function.body.statements[0], Statement::If(statement) if statement.otherwise.is_some())
    );
}

#[test]
fn newlines_inside_parentheses_are_whitespace() {
    let file =
        parsed("module m\nlet x = f(\n    a: 1,\n    b: [\n        2,\n        3\n    ]\n)\n");

    let Declaration::Binding(binding) = declarations(&file)[0] else {
        panic!("expected a binding");
    };
    assert!(
        matches!(&binding.value.kind, ExpressionKind::Call { arguments, .. } if arguments.len() == 2)
    );
}

#[test]
fn records_separate_fields_by_newlines_or_commas() {
    let file = parsed("module m\nlet a = { x: 1, y: 2 }\nlet b = {\n    x: 1\n    y: 2\n}\n");

    for declaration in declarations(&file) {
        let Declaration::Binding(binding) = declaration else {
            panic!("expected a binding");
        };
        assert!(matches!(&binding.value.kind, ExpressionKind::Record(fields) if fields.len() == 2));
    }
}

#[test]
fn checks_select_with_wildcards_skips_and_overrides() {
    let file = parsed(
        "module m\ncheck main {\n    run lemma.http.*\n    skip lemma.http.noisy\n    run worker.forwards { severity: warning }\n}\n",
    );

    let Declaration::Check(check) = declarations(&file)[0] else {
        panic!("expected a check");
    };
    assert!(
        matches!(&check.members[0], CheckMember::Run(run) if run.is_wildcard && run.target.dotted() == "lemma.http")
    );
    assert!(matches!(&check.members[1], CheckMember::Skip(_)));
    assert!(matches!(&check.members[2], CheckMember::Run(run) if run.overrides.len() == 1));
}

#[test]
fn a_reserved_word_is_a_name_only_in_backticks() {
    let reserved = parse("module m\nlet in = 1\n");
    let escaped = parse("module m\nlet `in` = 1\nlet x = python.module\n");

    assert!(reserved.diagnostics[0].message.contains("reserved word"));
    assert_eq!(escaped.diagnostics, []);
}

#[test]
fn chained_comparisons_are_reported() {
    let parsed = parse("module m\nlet x = a == b == c\n");

    assert_eq!(parsed.diagnostics.len(), 1);
    assert!(parsed.diagnostics[0].message.contains("do not chain"));
}

#[test]
fn a_broken_item_does_not_hide_the_ones_after_it() {
    let parsed = parse("module m\nlet = 3\nrelation R(a: Text)\nlet y = (1\nlet z = 2\n");

    let names: Vec<String> = declarations(&parsed.file)
        .iter()
        .filter_map(|declaration| declaration.name().map(|name| name.text.clone()))
        .collect();
    assert!(names.contains(&"R".to_owned()));
    assert_ne!(parsed.diagnostics.len(), 0);
}

#[test]
fn a_missing_module_line_is_reported() {
    let parsed = parse("let x = 1\n");

    assert!(parsed.diagnostics[0].message.contains("module"));
}

/// Wraps a spec fragment that lacks a `module` line.
fn as_module(fragment: &str) -> String {
    if fragment.trim_start().starts_with("module") || fragment.contains("\nmodule ") {
        return fragment.to_owned();
    }
    format!("module fragment\n{fragment}")
}

#[test]
fn every_code_block_of_the_specification_parses() {
    let corpus = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus");
    let mut entries: Vec<_> = std::fs::read_dir(corpus)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    entries.sort();
    let mut failures = Vec::new();
    for path in entries {
        let text = std::fs::read_to_string(&path).unwrap();
        let parsed = parse(&as_module(&text));
        if !parsed.diagnostics.is_empty() {
            failures.push(format!("{}: {:?}", path.display(), parsed.diagnostics));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
