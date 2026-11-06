use std::path::{Path, PathBuf};

use super::*;
use crate::syntax::parse;

/// The module's tree without positions, to compare two layouts of it.
fn tree_without_spans(source: &str) -> String {
    let parsed = parse(source);
    assert!(parsed.diagnostics.is_empty(), "{:#?}", parsed.diagnostics);
    let mut file = parsed.file;
    file.comments.clear();
    strip_spans(&format!("{file:?}"))
}

/// Removes every `Span { start: .., end: .. }` from a debug rendering.
fn strip_spans(debug: &str) -> String {
    let mut rest = debug;
    let mut kept = String::with_capacity(debug.len());
    while let Some(start) = rest.find("Span { start: ") {
        kept.push_str(&rest[..start]);
        let after = &rest[start..];
        let end = after.find('}').map_or(after.len(), |close| close + 1);
        rest = &after[end..];
    }
    kept.push_str(rest);
    kept
}

/// Formats `source`, checking what every layout must keep.
fn formatted(source: &str) -> String {
    let output = format(source).unwrap_or_else(|error| panic!("{error}: {source}"));
    assert_eq!(tree_without_spans(&output), tree_without_spans(source));
    assert_eq!(
        format(&output).as_deref(),
        Ok(output.as_str()),
        "not idempotent"
    );
    output
}

fn assert_formats(source: &str, expected: &str) {
    assert_eq!(formatted(source), expected);
}

#[test]
fn lines_are_indented_four_spaces_per_open_bracket() {
    assert_formats(
        "module m\n\nservice api {\nindex: { include: [\"src\"] }\n  export operation create {\n        dispatch: \"create\"\n}\n}\n",
        "module m\n\nservice api {\n    index: { include: [\"src\"] }\n    export operation create {\n        dispatch: \"create\"\n    }\n}\n",
    );
}

#[test]
fn brackets_opened_on_one_line_indent_once() {
    assert_formats(
        "module m\n\nlet x = f({\na: 1\n})\nlet y = [\n    g(\n  2,\n    ),\n]\n",
        "module m\n\nlet x = f({\n    a: 1\n})\nlet y = [\n    g(\n        2,\n    ),\n]\n",
    );
}

#[test]
fn a_closing_line_lines_up_with_the_line_that_opened_it() {
    assert_formats(
        "module m\n\nchecker c(\n    a: Operation,\n    b: Fact<Link>\n    ): Evidence<Json> {\n    command: [\"go\"]\n}\n",
        "module m\n\nchecker c(\n    a: Operation,\n    b: Fact<Link>\n): Evidence<Json> {\n    command: [\"go\"]\n}\n",
    );
}

#[test]
fn operators_arrows_and_bindings_have_a_space_on_each_side() {
    assert_formats(
        "module m\n\nlet a=1+2*3\nlet b = a==4&&!c||d!=-1\ngraph g:Link{\nA.x->B.y\n}\nrule r(x: Text) {\n(x):-q(x),x!=\"*\"\n}\n",
        "module m\n\nlet a = 1 + 2 * 3\nlet b = a == 4 && !c || d != -1\n\ngraph g : Link {\n    A.x -> B.y\n}\n\nrule r(x: Text) {\n    (x) :- q(x), x != \"*\"\n}\n",
    );
}

#[test]
fn colons_and_commas_are_followed_by_one_space() {
    assert_formats(
        "module m\n\nlet a = f(x :1 ,y:  [ 1,2 ])\nrelation R( a : Text,b:Int )\n",
        "module m\n\nlet a = f(x: 1, y: [1, 2])\nrelation R(a: Text, b: Int)\n",
    );
}

#[test]
fn a_graph_or_rule_names_its_relation_after_a_spaced_colon() {
    assert_formats(
        "module m\n\nrule reachable: DependsOn {\n    (a, b) :- d(a, b)\n}\n",
        "module m\n\nrule reachable : DependsOn {\n    (a, b) :- d(a, b)\n}\n",
    );
}

#[test]
fn type_arguments_are_tight_and_comparisons_are_not() {
    assert_formats(
        "module m\n\nfn f(link: Fact < Link >, n: Map<Text,List<Int>>): Bool {\n    return n<1 && n >2\n}\n",
        "module m\n\nfn f(link: Fact<Link>, n: Map<Text, List<Int>>): Bool {\n    return n < 1 && n > 2\n}\n",
    );
}

#[test]
fn members_calls_indexes_and_snapshots_are_tight() {
    assert_formats(
        "module m\n\nlet a = x . y ( 1 ) [ 2 ]\nrule r(o: Text) {\n    (o) :- operation @base (o), !operation @head (o)\n}\n",
        "module m\n\nlet a = x.y(1)[2]\n\nrule r(o: Text) {\n    (o) :- operation@base(o), !operation@head(o)\n}\n",
    );
}

#[test]
fn a_keyword_keeps_its_space_before_a_bracket() {
    assert_formats(
        "module m\n\nbehavior b {\n    for (k, v) in[1] {\n        expect(k)\n    }\n}\n",
        "module m\n\nbehavior b {\n    for (k, v) in [1] {\n        expect (k)\n    }\n}\n",
    );
}

#[test]
fn records_have_spaces_inside_their_braces_unless_empty() {
    assert_formats(
        "module m\n\nlet a = {x: 1}\nlet b = {  }\nlet c = f({y: 2})\n",
        "module m\n\nlet a = { x: 1 }\nlet b = {}\nlet c = f({ y: 2 })\n",
    );
}

#[test]
fn imports_and_wildcards_are_spaced_as_written_in_the_spec() {
    assert_formats(
        "module m\n\nimport *as go from lemma.go\nimport {Link,Other as Renamed} from lemma.links\n\ncheck main {\n    run lemma.links . *\n}\n",
        "module m\n\nimport * as go from lemma.go\nimport { Link, Other as Renamed } from lemma.links\n\ncheck main {\n    run lemma.links.*\n}\n",
    );
}

#[test]
fn a_name_list_spread_over_lines_is_indented_without_continuation() {
    assert_formats(
        "module m\n\nimport {\nLink,\nOther\n} from lemma.links\n",
        "module m\n\nimport {\n    Link,\n    Other\n} from lemma.links\n",
    );
}

#[test]
fn a_continued_line_is_indented_one_more_level() {
    assert_formats(
        "module m\n\nrule r(c: Text) {\n(c) :-\ncalls(c),\n!own(c)\n}\nlet total = 1 +\n2 +\n3\n",
        "module m\n\nrule r(c: Text) {\n    (c) :-\n        calls(c),\n        !own(c)\n}\n\nlet total = 1 +\n    2 +\n    3\n",
    );
}

#[test]
fn a_requirement_body_continues_after_when() {
    assert_formats(
        "module m\n\nrequire r {\nseverity: error\nwhen:\ncalls(c, op),\nremoved(op)\nmessage: \"{c}\"\n}\n",
        "module m\n\nrequire r {\n    severity: error\n    when:\n        calls(c, op),\n        removed(op)\n    message: \"{c}\"\n}\n",
    );
}

#[test]
fn because_continues_an_expect_and_else_does_not_indent() {
    assert_formats(
        "module m\n\nbehavior b {\n    expect a == b\n    because \"why\"\n    if a {\n        expect a\n    }\n    else {\n        expect b\n    }\n}\n",
        "module m\n\nbehavior b {\n    expect a == b\n        because \"why\"\n    if a {\n        expect a\n    }\n    else {\n        expect b\n    }\n}\n",
    );
}

#[test]
fn a_field_after_a_comma_is_not_a_continuation() {
    assert_formats(
        "module m\n\nentity E {\na: Text,\nb: List<Text>\nc: Int\n}\n",
        "module m\n\nentity E {\n    a: Text,\n    b: List<Text>\n    c: Int\n}\n",
    );
}

#[test]
fn a_negation_starting_a_line_stays_tight() {
    assert_formats(
        "module m\n\nrelation N(n: Int)\n\ngraph g : N {\n    - 1\n    2\n}\n",
        "module m\n\nrelation N(n: Int)\n\ngraph g : N {\n    -1\n    2\n}\n",
    );
}

#[test]
fn runs_of_blank_lines_become_one() {
    assert_formats(
        "\n\nmodule m\n\n\n\nlet a = 1\n\n\nlet b = 2\n\n\n",
        "module m\n\nlet a = 1\n\nlet b = 2\n",
    );
}

#[test]
fn blank_lines_at_the_edges_of_a_block_are_dropped() {
    assert_formats(
        "module m\n\nservice s {\n\n    a: 1\n\n    b: 2\n\n}\n",
        "module m\n\nservice s {\n    a: 1\n\n    b: 2\n}\n",
    );
}

#[test]
fn the_module_line_is_followed_by_a_blank_line() {
    assert_formats("module m\nlet a = 1\n", "module m\n\nlet a = 1\n");
}

#[test]
fn a_block_spanning_lines_is_set_off_by_blank_lines() {
    assert_formats(
        "module m\n\nlet a = 1\nlet b = 2\nservice s {\n    c: 3\n}\n// d's comment\nlet d = 4\n",
        "module m\n\nlet a = 1\nlet b = 2\n\nservice s {\n    c: 3\n}\n\n// d's comment\nlet d = 4\n",
    );
}

#[test]
fn bindings_and_one_line_blocks_may_be_grouped() {
    let grouped = "module m\n\nlet a = 1\nlet b = {\n    c: 2\n}\nrelation R(\n    a: Text\n)\nentity E { a: Text }\nentity F { b: Text }\n";

    assert_formats(grouped, grouped);
}

#[test]
fn comments_take_the_indentation_of_the_code_they_precede() {
    assert_formats(
        "module m\n\nservice s {\n// first\n        a: 1 // trailing\n  // last\n}\nrule r(c: Text) {\n    (c) :-\n    // why\n        q(c)\n}\n",
        "module m\n\nservice s {\n    // first\n    a: 1 // trailing\n    // last\n}\n\nrule r(c: Text) {\n    (c) :-\n        // why\n        q(c)\n}\n",
    );
}

#[test]
fn block_comments_are_kept_as_written() {
    assert_formats(
        "module m\n\n/* one\n   two */\nlet a = f(/* x */1)   /* y */\nlet b = /* z */ 2\n",
        "module m\n\n/* one\n   two */\nlet a = f(/* x */ 1) /* y */\nlet b = /* z */ 2\n",
    );
}

#[test]
fn trailing_whitespace_is_removed_and_the_file_ends_with_one_newline() {
    assert_formats(
        "module m   \n\nlet a = 1 // note   \n\t\nlet b = 2",
        "module m\n\nlet a = 1 // note\n\nlet b = 2\n",
    );
}

#[test]
fn semicolons_are_kept_where_the_author_put_them() {
    assert_formats(
        "module m\n\nlet a = 1 ;let b = 2\nservice s { a: 1 ; b: 2 }\n",
        "module m\n\nlet a = 1; let b = 2\nservice s { a: 1; b: 2 }\n",
    );
}

#[test]
fn every_comment_survives_in_order() {
    let source = "// head\nmodule m // after the name\n\n/* block */\nservice s { // opens\n    a: 1 /* inline */ // end\n    // inside\n}\n// tail\n";

    let output = formatted(source);

    let texts = |text: &str| -> Vec<String> {
        lex(text)
            .comments
            .iter()
            .map(|comment| comment.span.text(text).to_owned())
            .collect()
    };
    assert_eq!(texts(&output), texts(source));
}

#[test]
fn a_module_with_syntax_errors_is_refused() {
    let source = "module m\n\nservice s {\n    a: \n";

    let refused = format(source);

    let Err(FormatError::Syntax { diagnostics }) = refused else {
        panic!("expected a refusal, got {refused:?}");
    };
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.starts_with("expected")),
        "{diagnostics:#?}"
    );
}

#[test]
fn a_fragment_without_a_module_line_is_laid_out() {
    let formatted = format("let a=1\nlet b =  2\n");

    assert_eq!(formatted.as_deref(), Ok("let a = 1\nlet b = 2\n"));
}

#[test]
fn an_empty_module_stays_empty() {
    assert_eq!(format("module m\n").as_deref(), Ok("module m\n"));
}

#[test]
fn is_formatted_compares_with_the_canonical_layout() {
    assert_eq!(is_formatted("module m\n\nlet a = 1\n"), Ok(true));
    assert_eq!(is_formatted("module m\n\nlet a=1\n"), Ok(false));
}

fn verified(source: &str, formatted: &str) -> Result<(), FormatError> {
    let lexed = lex(source);
    verify(source, &lexed.tokens, &lexed.comments, formatted)
}

#[test]
fn a_layout_with_other_tokens_is_rejected() {
    let source = "module m\n\nlet a = 1\n";

    let checked = verified(source, "module m\n\nlet a = 2\n");

    assert_eq!(checked, Err(FormatError::Changed { offset: 18 }));
}

#[test]
fn a_layout_that_joins_two_lines_is_rejected() {
    let checked = verified("module m\nlet a = 1\n", "module m let a = 1\n");

    assert_eq!(checked, Err(FormatError::Changed { offset: 8 }));
}

#[test]
fn a_layout_that_drops_a_comment_is_rejected() {
    let checked = verified("module m // name\n", "module m\n");

    assert_eq!(checked, Err(FormatError::Changed { offset: 9 }));
}

#[test]
fn a_layout_differing_only_in_blank_lines_is_accepted() {
    let checked = verified("\nmodule m\nlet a = 1", "module m\n\n\nlet a = 1\n");

    assert_eq!(checked, Ok(()));
}

/// Wraps a spec fragment that lacks a `module` line, as the parser's corpus
/// test does; `None` when it has one.
fn as_module(fragment: &str) -> Option<String> {
    let has_module = fragment.trim_start().starts_with("module") || fragment.contains("\nmodule ");
    (!has_module).then(|| format!("module fragment\n\n{fragment}"))
}

/// Whether `text`, a module or a fragment of one, is in canonical form.
fn assert_canonical(name: &str, text: &str) {
    let module = as_module(text).unwrap_or_else(|| text.to_owned());
    let output = formatted(&module);
    assert_eq!(output, module, "{name} is not in canonical form");
}

fn lm_files(directory: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(directory)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|extension| extension == "lm"))
        .collect();
    files.sort();
    files
}

#[test]
fn the_standard_library_and_the_corpus_are_in_canonical_form() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = lm_files(&crate_root.join("src/stdlib"));
    files.extend(lm_files(&crate_root.join("tests/corpus")));
    assert!(files.len() > 20, "found {files:?}");

    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        assert_canonical(&path.display().to_string(), &text);
    }
}

/// `source` without indentation, with a space between and after every two
/// tokens and blank lines before it, line breaks and comments kept: the same
/// module, laid out badly.
fn mangled(source: &str) -> String {
    let lexed = lex(source);
    let mut pieces: Vec<(u32, &str)> = lexed
        .tokens
        .iter()
        .map(|token| (token.span.start, token.span.text(source)))
        .chain(
            lexed
                .comments
                .iter()
                .map(|comment| (comment.span.start, comment.span.text(source))),
        )
        .collect();
    pieces.sort_by_key(|(start, _)| *start);
    let mut text = String::from("\n\n");
    for (_, piece) in pieces {
        text.push_str(piece);
        if piece != "\n" {
            text.push(' ');
        }
    }
    text
}

#[test]
fn the_canonical_form_is_recovered_from_a_badly_laid_out_copy() {
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let mut files = lm_files(&crate_root.join("src/stdlib"));
    files.extend(lm_files(&crate_root.join("tests/corpus")));

    for path in files {
        let text = std::fs::read_to_string(&path).unwrap();
        let module = as_module(&text).unwrap_or(text);

        let output = formatted(&mangled(&module));

        assert_eq!(output, module, "{} was not recovered", path.display());
    }
}

/// The code blocks marked `lm` in a Markdown document.
fn lm_blocks(markdown: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        match current.as_mut() {
            None if line.trim() == "```lm" => current = Some(String::new()),
            None => {}
            Some(block) if line.trim() == "```" => {
                blocks.push(std::mem::take(block));
                current = None;
            }
            Some(block) => {
                block.push_str(line);
                block.push('\n');
            }
        }
    }
    blocks
}

#[test]
fn the_language_guides_examples_are_in_canonical_form() {
    let guide = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/language.md");
    let markdown = std::fs::read_to_string(guide).unwrap();
    let blocks = lm_blocks(&markdown);
    assert!(blocks.len() >= 4, "found {} blocks", blocks.len());

    for (number, block) in blocks.iter().enumerate() {
        assert_canonical(&format!("docs/language.md block {number}"), block);
    }
}
