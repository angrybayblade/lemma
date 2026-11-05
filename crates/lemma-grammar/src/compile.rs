//! Compiling a grammar file to a checked program.
//!
//! Entities and declared relations become input relations; a relation read
//! with `@base` also gets a base-snapshot input (`returns@base`). Rule heads
//! that are not declared become derived relations, their column types
//! inferred from the rule bodies. Each requirement becomes a derived
//! relation (`requirement:<name>`) whose rows are its findings.

use std::collections::{BTreeMap, BTreeSet};

use lemma_logic::{
    Atom, Column, ColumnType, FactSet, Literal, Program, Relation, RelationRole, Rule, Symbols,
    Term, Value,
};
use serde::Deserialize;

use crate::rules::{RawAtom, RawLiteral, RawRule, RawTerm, parse_body, parse_rule};
use crate::twins;
use crate::{DeclaredFact, GrammarError, GrammarFile, RelationSpec, SNAPSHOTS};

/// Sources a relation's facts may come from.
const SOURCES: [&str; 5] = ["syntax", "semantic", "spec", "declared", "observed"];
/// Type names every grammar understands, besides its entities.
const SYMBOLIC_TYPES: [&str; 2] = ["Text", "Symbol"];

/// How serious a requirement's findings are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// The change should not merge.
    #[default]
    Error,
    /// The change needs a look.
    Warning,
    /// Worth knowing.
    Info,
}

/// A compiled requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// Its name in the grammar.
    pub name: String,
    /// How serious its findings are.
    pub severity: Severity,
    /// Its message template, with `{variable}` placeholders.
    pub explain: Option<String>,
    /// The derived relation holding its findings.
    pub relation: String,
    /// The variables of each finding, in column order.
    pub variables: Vec<String>,
    /// For a requirement that reads a single snapshot, the derived relation
    /// holding what it finds in the base snapshot: a finding there too was
    /// not introduced by the change. `None` for a requirement comparing
    /// snapshots, whose findings are the change's by construction.
    pub base_relation: Option<String>,
}

/// What the grammar says about an input relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelationInfo {
    /// The column names, in order.
    pub columns: Vec<String>,
    /// Sources its facts may come from.
    pub sources: Vec<String>,
    /// What it means.
    pub description: Option<String>,
}

/// A grammar ready to evaluate.
#[derive(Debug, Clone)]
pub struct CompiledGrammar {
    /// The grammar's name.
    pub name: String,
    /// The checked program.
    pub program: Program,
    /// Requirements, sorted by name.
    pub requirements: Vec<Requirement>,
    /// Input relations by name (without snapshot suffix).
    pub inputs: BTreeMap<String, RelationInfo>,
    /// Facts declared in the grammar.
    pub facts: FactSet,
    /// Symbols interned so far; extractors keep using it.
    pub symbols: Symbols,
}

impl CompiledGrammar {
    /// The program relation holding an input relation's facts in a
    /// snapshot: the plain name for the head snapshot, `name@base` for the
    /// base snapshot. `None` if the program does not use that snapshot.
    pub fn input_relation(&self, relation: &str, snapshot: &str) -> Option<String> {
        let name = snapshot_name(relation, snapshot);
        self.program.relation(&name).map(|_| name)
    }
}

fn snapshot_name(relation: &str, snapshot: &str) -> String {
    if snapshot == "head" {
        relation.to_owned()
    } else {
        format!("{relation}@{snapshot}")
    }
}

/// Compiles a grammar file.
pub fn compile(file: &GrammarFile) -> Result<CompiledGrammar, GrammarError> {
    let entity_names: BTreeSet<String> = mapping_keys(&file.entities, "entities")?
        .into_iter()
        .collect();
    let mut inputs = BTreeMap::new();
    let mut columns: BTreeMap<String, Vec<Column>> = BTreeMap::new();
    for (name, value) in &file.entities {
        let name = key(name, "entities")?;
        let attributes = mapping_of(value, &format!("entity {name}"))?;
        let mut entity_columns = vec![symbol_column("id")];
        entity_columns.extend(attribute_columns(
            &attributes,
            &entity_names,
            &format!("entity {name}"),
        )?);
        declare(
            &mut inputs,
            &mut columns,
            &name,
            entity_columns,
            RelationSpec::default_for_entity(),
        )?;
    }
    for (name, value) in &file.relations {
        let name = key(name, "relations")?;
        let spec: RelationSpec = yaml_serde::from_value(value.clone())?;
        let relation_columns = relation_columns(&spec, &entity_names, &name)?;
        declare(&mut inputs, &mut columns, &name, relation_columns, spec)?;
    }
    let mut symbols = Symbols::new();
    let mut parsed = parse_all(file)?;
    // Read from the rules as written, before twins are added, so errors
    // name those rules.
    let mut base_inputs = base_snapshot_inputs(&parsed, &inputs)?;
    let twins = requirement_twins(&parsed, &inputs);
    // Inputs only twins read at the base: their declared facts hold in both
    // snapshots, as no rule of the grammar tells the snapshots apart.
    let twin_only: BTreeSet<String> = twins
        .base_inputs
        .difference(&base_inputs)
        .cloned()
        .collect();
    base_inputs.extend(twins.base_inputs.iter().cloned());
    parsed.rules.extend(twins.rules);
    let mut builder = Builder {
        inputs: &inputs,
        types: input_types(&columns, &base_inputs),
        symbols: &mut symbols,
    };
    let (rules, derived) = builder.rules(&parsed)?;
    let requirements = requirements(file, &parsed, &twins.relations)?;
    let relations = relations(&columns, &base_inputs, derived);
    let program = Program::new(relations, rules)?;
    let facts = declared_facts(&file.facts, &program, &twin_only, &mut symbols)?;
    Ok(CompiledGrammar {
        name: file.grammar.clone(),
        program,
        requirements,
        inputs,
        facts,
        symbols,
    })
}

impl RelationSpec {
    fn default_for_entity() -> Self {
        Self {
            args: Vec::new(),
            attributes: yaml_serde::Mapping::new(),
            sources: SOURCES.iter().map(|source| (*source).to_owned()).collect(),
            description: None,
        }
    }
}

fn declare(
    inputs: &mut BTreeMap<String, RelationInfo>,
    columns: &mut BTreeMap<String, Vec<Column>>,
    name: &str,
    relation_columns: Vec<Column>,
    spec: RelationSpec,
) -> Result<(), GrammarError> {
    let what = || format!("relation {name}");
    if let Some(source) = spec
        .sources
        .iter()
        .find(|source| !SOURCES.contains(&source.as_str()))
    {
        return Err(declaration(
            what(),
            &format!(
                "unknown source `{source}`; use one of {}",
                SOURCES.join(", ")
            ),
        ));
    }
    if name.contains('@') || name.starts_with("requirement:") {
        return Err(declaration(
            what(),
            "names cannot contain `@` or start with `requirement:`",
        ));
    }
    let info = RelationInfo {
        columns: relation_columns
            .iter()
            .map(|column| column.name.clone())
            .collect(),
        sources: spec.sources,
        description: spec.description,
    };
    if inputs.insert(name.to_owned(), info).is_some() {
        return Err(declaration(
            what(),
            "declared twice (entities and relations share names)",
        ));
    }
    columns.insert(name.to_owned(), relation_columns);
    Ok(())
}

fn relation_columns(
    spec: &RelationSpec,
    entities: &BTreeSet<String>,
    name: &str,
) -> Result<Vec<Column>, GrammarError> {
    let what = format!("relation {name}");
    let mut used: BTreeMap<String, usize> = BTreeMap::new();
    let mut relation_columns = Vec::new();
    for kind in &spec.args {
        let column_type = column_type(kind, entities, &what)?;
        let base = kind.to_lowercase();
        let count = used.entry(base.clone()).or_insert(0);
        *count += 1;
        let column_name = if *count == 1 {
            base
        } else {
            format!("{base}{count}")
        };
        relation_columns.push(Column {
            name: column_name,
            kind: column_type,
        });
    }
    relation_columns.extend(attribute_columns(&spec.attributes, entities, &what)?);
    if relation_columns.is_empty() {
        return Err(declaration(
            what,
            "needs at least one argument or attribute",
        ));
    }
    Ok(relation_columns)
}

fn attribute_columns(
    attributes: &yaml_serde::Mapping,
    entities: &BTreeSet<String>,
    what: &str,
) -> Result<Vec<Column>, GrammarError> {
    let mut result = Vec::new();
    for (name, kind) in attributes {
        let name = key(name, what)?;
        let kind = kind.as_str().ok_or_else(|| {
            declaration(
                what.to_owned(),
                &format!("attribute `{name}` needs a type name"),
            )
        })?;
        result.push(Column {
            name,
            kind: column_type(kind, entities, what)?,
        });
    }
    Ok(result)
}

fn column_type(
    name: &str,
    entities: &BTreeSet<String>,
    what: &str,
) -> Result<ColumnType, GrammarError> {
    if name == "Number" {
        return Ok(ColumnType::Number);
    }
    if SYMBOLIC_TYPES.contains(&name) || entities.contains(name) {
        return Ok(ColumnType::Symbol);
    }
    Err(GrammarError::UnknownType {
        what: what.to_owned(),
        name: name.to_owned(),
    })
}

fn symbol_column(name: &str) -> Column {
    Column {
        name: name.to_owned(),
        kind: ColumnType::Symbol,
    }
}

/// Rules and requirement bodies, parsed, each with its source text.
struct Parsed {
    rules: Vec<(String, RawRule)>,
    requirements: Vec<(String, String, Vec<RawLiteral>)>,
}

fn parse_all(file: &GrammarFile) -> Result<Parsed, GrammarError> {
    let mut rules = Vec::new();
    for text in &file.rules {
        let rule = parse_rule(text).map_err(|source| parse_error(text, source))?;
        rules.push((text.clone(), rule));
    }
    let mut requirements = Vec::new();
    for (name, spec) in &file.requirements {
        let body = parse_body(&spec.when).map_err(|source| parse_error(&spec.when, source))?;
        requirements.push((name.clone(), spec.when.clone(), body));
    }
    Ok(Parsed {
        rules,
        requirements,
    })
}

fn parse_error(text: &str, source: crate::ParseError) -> GrammarError {
    GrammarError::Parse {
        text: text.to_owned(),
        source,
    }
}

/// Input relations some rule reads in the base snapshot.
fn base_snapshot_inputs(
    parsed: &Parsed,
    inputs: &BTreeMap<String, RelationInfo>,
) -> Result<BTreeSet<String>, GrammarError> {
    let mut base = BTreeSet::new();
    let bodies = parsed
        .rules
        .iter()
        .map(|(text, rule)| (text, &rule.body))
        .chain(
            parsed
                .requirements
                .iter()
                .map(|(_, text, body)| (text, body)),
        );
    for (text, body) in bodies {
        for atom in body.iter().filter_map(literal_atom) {
            let Some(snapshot) = &atom.snapshot else {
                continue;
            };
            if !SNAPSHOTS.contains(&snapshot.as_str()) {
                return Err(rule_error(
                    text,
                    &format!("unknown snapshot `@{snapshot}`; use @base or @head"),
                ));
            }
            if !inputs.contains_key(&atom.relation) {
                return Err(rule_error(
                    text,
                    &format!("`{}` is derived; only inputs have snapshots", atom.relation),
                ));
            }
            if snapshot == "base" {
                base.insert(atom.relation.clone());
            }
        }
    }
    Ok(base)
}

pub(crate) fn literal_atom(literal: &RawLiteral) -> Option<&RawAtom> {
    match literal {
        RawLiteral::Positive(atom) | RawLiteral::Negative(atom) => Some(atom),
        RawLiteral::Compare(..) => None,
    }
}

fn input_types(
    columns: &BTreeMap<String, Vec<Column>>,
    base: &BTreeSet<String>,
) -> BTreeMap<String, Vec<ColumnType>> {
    let mut types = BTreeMap::new();
    for (name, relation_columns) in columns {
        let kinds: Vec<ColumnType> = relation_columns.iter().map(|column| column.kind).collect();
        if base.contains(name) {
            types.insert(snapshot_name(name, "base"), kinds.clone());
        }
        types.insert(name.clone(), kinds);
    }
    types
}

/// Converts parsed rules, inferring the columns of derived relations.
struct Builder<'a> {
    inputs: &'a BTreeMap<String, RelationInfo>,
    /// Column types of every relation known so far, by program name.
    types: BTreeMap<String, Vec<ColumnType>>,
    symbols: &'a mut Symbols,
}

impl Builder<'_> {
    /// Rules (requirements included) and the derived relations they define.
    fn rules(&mut self, parsed: &Parsed) -> Result<(Vec<Rule>, Vec<Relation>), GrammarError> {
        let mut raw: Vec<(String, RawRule)> = parsed.rules.clone();
        for (name, text, body) in &parsed.requirements {
            raw.push((text.clone(), requirement_rule(name, body)));
        }
        let derived = self.infer_derived(&raw)?;
        let rules = raw
            .iter()
            .map(|(text, rule)| self.rule(text, rule))
            .collect::<Result<_, _>>()?;
        Ok((rules, derived))
    }

    /// Types each derived relation from the first rule whose body types all
    /// its head's variables, repeating until nothing changes.
    fn infer_derived(&mut self, raw: &[(String, RawRule)]) -> Result<Vec<Relation>, GrammarError> {
        let mut derived = BTreeMap::new();
        while self.infer_pass(raw, &mut derived)? {}
        if let Some((text, rule)) = raw
            .iter()
            .find(|(_, rule)| !derived.contains_key(&rule.head.relation))
        {
            let message = format!(
                "cannot work out the columns of `{}` from its rules",
                rule.head.relation
            );
            return Err(rule_error(text, &message));
        }
        Ok(derived.into_values().collect())
    }

    /// One pass over the rules; returns whether a relation got typed.
    fn infer_pass(
        &mut self,
        raw: &[(String, RawRule)],
        derived: &mut BTreeMap<String, Relation>,
    ) -> Result<bool, GrammarError> {
        let mut progressed = false;
        for (text, rule) in raw {
            let head = &rule.head.relation;
            if self.inputs.contains_key(head) {
                return Err(rule_error(
                    text,
                    &format!("`{head}` is an input; rules derive new relations"),
                ));
            }
            if derived.contains_key(head) {
                continue;
            }
            let Some(relation) = self.head_relation(rule) else {
                continue;
            };
            let kinds = relation.columns.iter().map(|column| column.kind).collect();
            self.types.insert(head.clone(), kinds);
            derived.insert(head.clone(), relation);
            progressed = true;
        }
        Ok(progressed)
    }

    fn head_relation(&self, rule: &RawRule) -> Option<Relation> {
        let types = self.variable_types(&rule.body);
        let mut used: BTreeMap<String, usize> = BTreeMap::new();
        let columns = rule
            .head
            .terms
            .iter()
            .enumerate()
            .map(|(position, term)| {
                let (name, kind) = match term {
                    RawTerm::Variable(name) => (name.clone(), *types.get(name)?),
                    RawTerm::Text(_) => (format!("c{position}"), ColumnType::Symbol),
                    RawTerm::Number(_) => (format!("c{position}"), ColumnType::Number),
                    RawTerm::Wildcard => return None,
                };
                let count = used.entry(name.clone()).or_insert(0);
                *count += 1;
                let name = if *count == 1 {
                    name
                } else {
                    format!("{name}{count}")
                };
                Some(Column { name, kind })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Relation {
            name: rule.head.relation.clone(),
            columns,
            role: RelationRole::Derived,
        })
    }

    /// Types of variables bound by positive atoms over relations typed so far.
    fn variable_types(&self, body: &[RawLiteral]) -> BTreeMap<String, ColumnType> {
        let mut types = BTreeMap::new();
        for literal in body {
            let RawLiteral::Positive(atom) = literal else {
                continue;
            };
            let Some(kinds) = self.types.get(&self.program_name(atom)) else {
                continue;
            };
            bind_types(&atom.terms, kinds, &mut types);
        }
        types
    }

    fn program_name(&self, atom: &RawAtom) -> String {
        match atom.snapshot.as_deref() {
            Some(snapshot) if self.inputs.contains_key(&atom.relation) => {
                snapshot_name(&atom.relation, snapshot)
            }
            _ => atom.relation.clone(),
        }
    }

    fn rule(&mut self, text: &str, rule: &RawRule) -> Result<Rule, GrammarError> {
        let head = self.atom(text, &rule.head)?;
        let body = rule
            .body
            .iter()
            .map(|literal| {
                Ok(match literal {
                    RawLiteral::Positive(atom) => Literal::Positive(self.atom(text, atom)?),
                    RawLiteral::Negative(atom) => Literal::Negative(self.atom(text, atom)?),
                    RawLiteral::Compare(left, op, right) => Literal::Compare {
                        left: self.term(left),
                        op: *op,
                        right: self.term(right),
                    },
                })
            })
            .collect::<Result<_, GrammarError>>()?;
        Ok(Rule { head, body })
    }

    fn atom(&mut self, text: &str, atom: &RawAtom) -> Result<Atom, GrammarError> {
        let relation = self.program_name(atom);
        if !self.types.contains_key(&relation) {
            return Err(rule_error(
                text,
                &format!("unknown relation `{}`", atom.relation),
            ));
        }
        Ok(Atom {
            relation,
            terms: atom.terms.iter().map(|term| self.term(term)).collect(),
        })
    }

    fn term(&mut self, term: &RawTerm) -> Term {
        match term {
            RawTerm::Variable(name) => Term::Variable(name.clone()),
            RawTerm::Wildcard => Term::Wildcard,
            RawTerm::Text(text) => Term::Constant(self.symbols.intern(text)),
            RawTerm::Number(number) => Term::Constant(Value::Number(*number)),
        }
    }
}

/// Records the column type of each variable in `terms`, keeping the first.
fn bind_types(terms: &[RawTerm], kinds: &[ColumnType], types: &mut BTreeMap<String, ColumnType>) {
    for (term, kind) in terms.iter().zip(kinds) {
        if let RawTerm::Variable(name) = term {
            types.entry(name.clone()).or_insert(*kind);
        }
    }
}

/// A requirement as a rule deriving one row per match, carrying every
/// variable its positive conditions bind, in order of appearance.
fn requirement_rule(name: &str, body: &[RawLiteral]) -> RawRule {
    let variables = requirement_variables(body);
    RawRule {
        head: RawAtom {
            relation: format!("requirement:{name}"),
            snapshot: None,
            terms: variables.into_iter().map(RawTerm::Variable).collect(),
        },
        body: body.to_vec(),
    }
}

fn requirement_variables(body: &[RawLiteral]) -> Vec<String> {
    let mut variables = Vec::new();
    for literal in body {
        let RawLiteral::Positive(atom) = literal else {
            continue;
        };
        for term in &atom.terms {
            if let RawTerm::Variable(name) = term
                && !variables.contains(name)
            {
                variables.push(name.clone());
            }
        }
    }
    variables
}

/// Base-snapshot twins of the requirements that read a single snapshot.
fn requirement_twins(parsed: &Parsed, inputs: &BTreeMap<String, RelationInfo>) -> twins::Twins {
    let mut rules = parsed.rules.clone();
    let mut names = Vec::new();
    for (name, text, body) in &parsed.requirements {
        let rule = requirement_rule(name, body);
        names.push(rule.head.relation.clone());
        rules.push((text.clone(), rule));
    }
    twins::base_twins(&rules, &names, inputs)
}

fn requirements(
    file: &GrammarFile,
    parsed: &Parsed,
    twins: &BTreeMap<String, String>,
) -> Result<Vec<Requirement>, GrammarError> {
    parsed
        .requirements
        .iter()
        .map(|(name, text, body)| {
            let spec = &file.requirements[name];
            let variables = requirement_variables(body);
            if variables.is_empty() {
                return Err(rule_error(text, "a requirement needs a positive condition"));
            }
            let relation = format!("requirement:{name}");
            Ok(Requirement {
                name: name.clone(),
                severity: spec.severity,
                explain: spec.explain.clone(),
                base_relation: twins.get(&relation).cloned(),
                relation,
                variables,
            })
        })
        .collect()
}

fn relations(
    columns: &BTreeMap<String, Vec<Column>>,
    base: &BTreeSet<String>,
    derived: Vec<Relation>,
) -> Vec<Relation> {
    let mut relations = Vec::new();
    for (name, relation_columns) in columns {
        if base.contains(name) {
            relations.push(Relation {
                name: snapshot_name(name, "base"),
                columns: relation_columns.clone(),
                role: RelationRole::Input,
            });
        }
        relations.push(Relation {
            name: name.clone(),
            columns: relation_columns.clone(),
            role: RelationRole::Input,
        });
    }
    relations.extend(derived);
    relations
}

/// The grammar's own facts. A fact names its snapshot or is in the head.
/// For a relation in `twin_only` given no base facts of its own, the head's
/// are the base's too: the grammar declared it as constant.
fn declared_facts(
    declared: &[DeclaredFact],
    program: &Program,
    twin_only: &BTreeSet<String>,
    symbols: &mut Symbols,
) -> Result<FactSet, GrammarError> {
    let with_base: BTreeSet<&str> = declared
        .iter()
        .filter(|fact| fact.snapshot.as_deref() == Some("base"))
        .map(|fact| fact.relation.as_str())
        .collect();
    let mut facts = FactSet::new();
    for fact in declared {
        let snapshot = fact.snapshot.as_deref().unwrap_or("head");
        let name = snapshot_name(&fact.relation, snapshot);
        facts.insert(&name, declared_row(fact, program, &name, symbols)?);
        let is_constant =
            twin_only.contains(&fact.relation) && !with_base.contains(fact.relation.as_str());
        if fact.snapshot.is_none() && is_constant {
            let base = snapshot_name(&fact.relation, "base");
            facts.insert(&base, declared_row(fact, program, &base, symbols)?);
        }
    }
    Ok(facts)
}

/// One declared fact as a row of the program relation `name`.
fn declared_row(
    fact: &DeclaredFact,
    program: &Program,
    name: &str,
    symbols: &mut Symbols,
) -> Result<Vec<Value>, GrammarError> {
    let what = || format!("fact for {}", fact.relation);
    let relation = program
        .relation(name)
        .filter(|relation| relation.role == RelationRole::Input)
        .ok_or_else(|| declaration(what(), "no such input relation in this snapshot"))?;
    if relation.columns.len() != fact.values.len() {
        return Err(declaration(
            what(),
            &format!("needs {} values", relation.columns.len()),
        ));
    }
    relation
        .columns
        .iter()
        .zip(&fact.values)
        .map(|(column, value)| match (column.kind, value) {
            (ColumnType::Symbol, yaml_serde::Value::String(text)) => Ok(symbols.intern(text)),
            (ColumnType::Number, yaml_serde::Value::Number(number)) => number
                .as_i64()
                .map(Value::Number)
                .ok_or_else(|| declaration(what(), "numbers must be integers")),
            _ => Err(declaration(
                what(),
                &format!("`{}` expects a {:?}", column.name, column.kind),
            )),
        })
        .collect()
}

fn mapping_keys(mapping: &yaml_serde::Mapping, what: &str) -> Result<Vec<String>, GrammarError> {
    mapping.keys().map(|name| key(name, what)).collect()
}

fn key(value: &yaml_serde::Value, what: &str) -> Result<String, GrammarError> {
    value
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| declaration(what.to_owned(), "names must be strings"))
}

fn mapping_of(value: &yaml_serde::Value, what: &str) -> Result<yaml_serde::Mapping, GrammarError> {
    match value {
        yaml_serde::Value::Mapping(mapping) => Ok(mapping.clone()),
        yaml_serde::Value::Null => Ok(yaml_serde::Mapping::new()),
        _ => Err(declaration(
            what.to_owned(),
            "expected a map of attribute names to types",
        )),
    }
}

fn declaration(what: String, message: &str) -> GrammarError {
    GrammarError::Declaration {
        what,
        message: message.to_owned(),
    }
}

fn rule_error(text: &str, message: &str) -> GrammarError {
    GrammarError::Rule {
        text: text.to_owned(),
        message: message.to_owned(),
    }
}
