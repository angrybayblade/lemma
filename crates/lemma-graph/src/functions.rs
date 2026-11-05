//! A repository's functions as tables over interned names (spec 12), the
//! form tracing walks: every name, file and identifier is stored once and
//! referred to by its position in one sorted table, so reading a
//! repository allocates per distinct name rather than per mention, and a
//! walk compares integers.
//!
//! Built from functions in the order their files are given, which is the
//! order a walk visits them in.

use std::collections::HashMap;

use crate::impact::Function;

/// A name's position in a table's sorted strings.
pub(crate) type NameId = u32;

/// Marks a function without a container type.
const NO_NAME: NameId = NameId::MAX;

/// What a function is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// A file's code outside its functions.
    TopLevel,
    /// A function.
    Function,
    /// A method.
    Method,
    /// A constant, such as one holding a route's registration.
    Constant,
    /// A variable.
    Variable,
    /// A top-level destructuring (`const { handler } = make()`), reached
    /// through the names it binds.
    Destructuring,
}

impl Kind {
    /// Its name in output: empty for top-level code.
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::TopLevel => "",
            Self::Function => "function",
            Self::Method => "method",
            Self::Constant => "constant",
            Self::Variable => "variable",
            Self::Destructuring => "destructuring",
        }
    }

    /// The kind `text` names, as [`Kind::as_str`] writes it; top-level
    /// code for anything else.
    pub(crate) fn from_name(text: &str) -> Self {
        match text {
            "function" => Self::Function,
            "method" => Self::Method,
            "constant" => Self::Constant,
            "variable" => Self::Variable,
            "destructuring" => Self::Destructuring,
            _ => Self::TopLevel,
        }
    }
}

/// A repository's functions, in the order their files were given.
#[derive(Debug, Default)]
pub(crate) struct Functions {
    /// Every file, name and identifier, sorted and distinct.
    names: Vec<Box<str>>,
    file: Vec<NameId>,
    name: Vec<NameId>,
    qualified: Vec<NameId>,
    /// The type a method belongs to, the last part of its container
    /// (`Tools` for `pkg.Tools.execute`); [`NO_NAME`] for others.
    container: Vec<NameId>,
    kind: Vec<Kind>,
    lines: Vec<(u32, u32)>,
    /// Where each function's mentions end in `mentions`; sorted per
    /// function, so in the order of their text.
    mention_ends: Vec<u32>,
    mentions: Vec<NameId>,
    bind_ends: Vec<u32>,
    binds: Vec<NameId>,
    /// The names each function is reached by, worked out on construction:
    /// see [`Functions::callable_names`].
    callable_ends: Vec<u32>,
    callable: Vec<NameId>,
}

impl Functions {
    /// A table of `functions`, in their order.
    pub(crate) fn from_functions(functions: &[Function]) -> Self {
        let mut builder = Builder::default();
        for function in functions {
            builder.add_function(function);
        }
        builder.finish()
    }

    /// How many functions there are.
    pub(crate) fn len(&self) -> usize {
        self.name.len()
    }

    /// The text of a name.
    pub(crate) fn text(&self, name: NameId) -> &str {
        &self.names[name as usize]
    }

    /// The id of `text`, if any function's file, name or mentions hold it.
    pub(crate) fn lookup(&self, text: &str) -> Option<NameId> {
        let found = self
            .names
            .binary_search_by(|name| (**name).cmp(text))
            .ok()?;
        NameId::try_from(found).ok()
    }

    /// How many distinct names the table holds.
    pub(crate) fn name_count(&self) -> usize {
        self.names.len()
    }

    /// The file a function is in.
    pub(crate) fn file(&self, function: usize) -> NameId {
        self.file[function]
    }

    /// The file a function is in, as text.
    pub(crate) fn file_text(&self, function: usize) -> &str {
        self.text(self.file[function])
    }

    /// A function's own name; empty for top-level code.
    pub(crate) fn name(&self, function: usize) -> NameId {
        self.name[function]
    }

    /// A function's name prefixed by its containers, as text.
    pub(crate) fn qualified_text(&self, function: usize) -> &str {
        self.text(self.qualified[function])
    }

    /// What a function is.
    pub(crate) fn kind(&self, function: usize) -> Kind {
        self.kind[function]
    }

    /// A function's first and last lines, counting from 1.
    pub(crate) fn lines(&self, function: usize) -> (u32, u32) {
        self.lines[function]
    }

    /// Whether a function is top-level code, which has no name.
    pub(crate) fn is_unnamed(&self, function: usize) -> bool {
        self.text(self.name[function]).is_empty()
    }

    /// The identifiers a function's code mentions, in the order of their
    /// text.
    pub(crate) fn mentions(&self, function: usize) -> &[NameId] {
        range(&self.mention_ends, &self.mentions, function)
    }

    /// The names a function's code binds without defining them.
    pub(crate) fn binds(&self, function: usize) -> &[NameId] {
        range(&self.bind_ends, &self.binds, function)
    }

    /// The names a function is reached by: its own, then for a method its
    /// type's, since callers often hold the type (a constructor, a handler
    /// object) rather than name the method; for top-level code, the names
    /// it binds.
    pub(crate) fn callable_names(&self, function: usize) -> &[NameId] {
        range(&self.callable_ends, &self.callable, function)
    }

    fn work_out_callable_names(&mut self) {
        let mut ends = Vec::with_capacity(self.len());
        let mut callable = Vec::with_capacity(self.len() * 2);
        for function in 0..self.len() {
            self.push_reached_by(function, &mut callable);
            ends.push(end(callable.len()));
        }
        self.callable_ends = ends;
        self.callable = callable;
    }

    /// Pushes onto `names` the names a function is reached by, as
    /// [`Functions::callable_names`] keeps them.
    fn push_reached_by(&self, function: usize, names: &mut Vec<NameId>) {
        if self.is_unnamed(function) {
            names.extend_from_slice(self.binds(function));
            return;
        }
        names.push(self.name[function]);
        let container = self.container[function];
        if container != NO_NAME {
            names.push(container);
        }
    }
}

/// The slice of `values` for item `index`, given where each item ends.
fn range<'a>(ends: &[u32], values: &'a [NameId], index: usize) -> &'a [NameId] {
    let start = if index == 0 {
        0
    } else {
        ends[index - 1] as usize
    };
    &values[start..ends[index] as usize]
}

/// A length as an end offset. Tables stay far below 4 billion entries.
fn end(length: usize) -> u32 {
    u32::try_from(length).unwrap_or(u32::MAX)
}

/// Builds a table, interning names as functions are added and sorting
/// them at the end.
#[derive(Default)]
pub(crate) struct Builder {
    /// Names in the order first seen, and each one's position there.
    seen: Vec<Box<str>>,
    positions: HashMap<Box<str>, NameId>,
    table: Functions,
}

impl Builder {
    fn intern(&mut self, text: &str) -> NameId {
        if let Some(position) = self.positions.get(text) {
            return *position;
        }
        let position = end(self.seen.len());
        self.seen.push(Box::from(text));
        self.positions.insert(Box::from(text), position);
        position
    }

    /// The type a method belongs to, from its qualified name.
    fn container(&mut self, name: &str, qualified: &str) -> NameId {
        if name.is_empty() {
            return NO_NAME;
        }
        let Some((container, _)) = qualified.rsplit_once('.') else {
            return NO_NAME;
        };
        self.intern(container.rsplit('.').next().unwrap_or(container))
    }

    /// Adds one function.
    pub(crate) fn add_function(&mut self, function: &Function) {
        let file = self.intern(&function.file);
        let name = self.intern(&function.name);
        let qualified = self.intern(&function.qualified_name);
        let container = self.container(&function.name, &function.qualified_name);
        let mentions: Vec<NameId> = function
            .mentions
            .iter()
            .map(|text| self.intern(text))
            .collect();
        let binds: Vec<NameId> = function
            .binds
            .iter()
            .map(|text| self.intern(text))
            .collect();
        let table = &mut self.table;
        table.file.push(file);
        table.name.push(name);
        table.qualified.push(qualified);
        table.container.push(container);
        table.kind.push(Kind::from_name(&function.kind));
        table.lines.push(function.lines);
        table.mentions.extend(mentions);
        table.mention_ends.push(end(table.mentions.len()));
        table.binds.extend(binds);
        table.bind_ends.push(end(table.binds.len()));
    }

    /// The table, its names sorted so ids follow their text's order.
    pub(crate) fn finish(self) -> Functions {
        let Self {
            seen, mut table, ..
        } = self;
        let mut order: Vec<usize> = (0..seen.len()).collect();
        order.sort_unstable_by(|left, right| seen[*left].cmp(&seen[*right]));
        let mut rank = vec![0; seen.len()];
        for (sorted, first_seen) in order.iter().enumerate() {
            rank[*first_seen] = end(sorted);
        }
        let mut seen: Vec<Option<Box<str>>> = seen.into_iter().map(Some).collect();
        table.names = order
            .iter()
            .map(|first_seen| seen[*first_seen].take().unwrap_or_default())
            .collect();
        let renumber = |ids: &mut Vec<NameId>| {
            for id in ids.iter_mut() {
                *id = rank[*id as usize];
            }
        };
        renumber(&mut table.file);
        renumber(&mut table.name);
        renumber(&mut table.qualified);
        renumber(&mut table.mentions);
        renumber(&mut table.binds);
        for id in &mut table.container {
            if *id != NO_NAME {
                *id = rank[*id as usize];
            }
        }
        // Mentions arrive in text order, which renumbering keeps. A walk's
        // choices follow that order, so it is restored rather than
        // assumed; runs already sorted cost little.
        for function in 0..table.len() {
            let start = if function == 0 {
                0
            } else {
                table.mention_ends[function - 1] as usize
            };
            table.mentions[start..table.mention_ends[function] as usize].sort_unstable();
        }
        table.work_out_callable_names();
        table
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    fn function(file: &str, name: &str, qualified: &str, mentions: &[&str]) -> Function {
        Function {
            file: file.to_owned(),
            name: name.to_owned(),
            qualified_name: qualified.to_owned(),
            kind: "method".to_owned(),
            lines: (3, 9),
            mentions: mentions.iter().map(|text| (*text).to_owned()).collect(),
            binds: Vec::new(),
        }
    }

    fn texts(functions: &Functions, ids: &[NameId]) -> Vec<String> {
        ids.iter()
            .map(|id| functions.text(*id).to_owned())
            .collect()
    }

    #[test]
    fn a_method_is_reached_by_its_name_and_its_type() {
        let functions =
            Functions::from_functions(&[function("a.go", "Run", "pkg.Tools.Run", &["x"])]);

        let callable = texts(&functions, functions.callable_names(0));

        assert_eq!(callable, ["Run", "Tools"]);
    }

    #[test]
    fn names_are_looked_up_by_their_text() {
        let functions = Functions::from_functions(&[function("a.ts", "run", "run", &["go"])]);

        let found: BTreeSet<&str> = ["go", "run", "a.ts", "missing"]
            .into_iter()
            .filter(|text| functions.lookup(text).is_some())
            .collect();

        assert_eq!(found, BTreeSet::from(["a.ts", "go", "run"]));
    }
}
