//! A repository's functions as tables over interned names (spec 12), the
//! form tracing walks: every name, file and identifier is stored once and
//! referred to by its position in one sorted table, so reading a
//! repository allocates per distinct name rather than per mention, and a
//! walk compares integers.
//!
//! Two encodings: a file's record, one parse's functions with the file's
//! own string table, kept per file in the unit cache; and a segment, a
//! whole repository's table, kept whole so an unchanged repository is one
//! read. Records merge into a table in the order their files are given,
//! which is the order a walk visits functions in.

use std::collections::HashMap;

use lemma_legacy_store::{RecordError, RecordReader, RecordWriter, StringTable};

use crate::impact::Function;

/// A name's position in a table's sorted strings.
pub(crate) type NameId = u32;

/// Marks a function without a container type.
const NO_NAME: NameId = NameId::MAX;

/// A file record's kind and layout version.
const RECORD_MAGIC: [u8; 4] = *b"LMFR";
const RECORD_VERSION: u16 = 1;

/// A segment's kind and layout version.
const SEGMENT_MAGIC: [u8; 4] = *b"LMFS";
const SEGMENT_VERSION: u16 = 1;

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

    fn code(self) -> u8 {
        match self {
            Self::TopLevel => 0,
            Self::Function => 1,
            Self::Method => 2,
            Self::Constant => 3,
            Self::Variable => 4,
            Self::Destructuring => 5,
        }
    }

    fn from_code(code: u8) -> Option<Self> {
        Some(match code {
            0 => Self::TopLevel,
            1 => Self::Function,
            2 => Self::Method,
            3 => Self::Constant,
            4 => Self::Variable,
            5 => Self::Destructuring,
            _ => return None,
        })
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
    #[cfg(test)]
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

    /// The whole table as a segment.
    pub(crate) fn encode_segment(&self) -> Vec<u8> {
        let mut writer = RecordWriter::new(SEGMENT_MAGIC, SEGMENT_VERSION);
        writer.strings(&self.names);
        writer.column(&self.file);
        writer.column(&self.name);
        writer.column(&self.qualified);
        writer.column(&self.container);
        writer.bytes(&self.kind.iter().map(|kind| kind.code()).collect::<Vec<_>>());
        writer.column(&flatten_lines(&self.lines));
        writer.column(&self.mention_ends);
        writer.column(&self.mentions);
        writer.column(&self.bind_ends);
        writer.column(&self.binds);
        writer.finish()
    }

    /// The table a segment holds, checked before it is trusted: every
    /// column the same length, every id inside the table.
    pub(crate) fn decode_segment(bytes: &[u8]) -> Result<Self, RecordError> {
        let mut reader = RecordReader::open(bytes, SEGMENT_MAGIC, SEGMENT_VERSION)?;
        let names: Vec<Box<str>> = reader.strings()?.into_iter().map(Box::from).collect();
        let file = reader.column()?;
        let name = reader.column()?;
        let qualified = reader.column()?;
        let container = reader.column()?;
        let kind = decode_kinds(reader.bytes()?)?;
        let lines = unflatten_lines(&reader.column()?)?;
        let mention_ends = reader.column()?;
        let mentions = reader.column()?;
        let bind_ends = reader.column()?;
        let binds = reader.column()?;
        reader.finish()?;
        let count = file.len();
        let same_length = [name.len(), qualified.len(), container.len(), kind.len()]
            .iter()
            .chain([lines.len(), mention_ends.len(), bind_ends.len()].iter())
            .all(|length| *length == count);
        if !same_length {
            return Err(RecordError::OutOfRange { field: "columns" });
        }
        check_ends(&mention_ends, mentions.len(), "mention ends")?;
        check_ends(&bind_ends, binds.len(), "bind ends")?;
        let limit = names.len();
        for (column, field) in [
            (&file, "file"),
            (&name, "name"),
            (&qualified, "qualified"),
            (&mentions, "mentions"),
            (&binds, "binds"),
        ] {
            check_ids(column, limit, field)?;
        }
        let containers = container.iter().filter(|id| **id != NO_NAME);
        if containers.into_iter().any(|id| *id as usize >= limit) {
            return Err(RecordError::OutOfRange { field: "container" });
        }
        let mut functions = Self {
            names,
            file,
            name,
            qualified,
            container,
            kind,
            lines,
            mention_ends,
            mentions,
            bind_ends,
            binds,
            callable_ends: Vec::new(),
            callable: Vec::new(),
        };
        functions.work_out_callable_names();
        Ok(functions)
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

fn flatten_lines(lines: &[(u32, u32)]) -> Vec<u32> {
    lines
        .iter()
        .flat_map(|(first, last)| [*first, *last])
        .collect()
}

fn unflatten_lines(flat: &[u32]) -> Result<Vec<(u32, u32)>, RecordError> {
    if flat.len() % 2 != 0 {
        return Err(RecordError::OutOfRange { field: "lines" });
    }
    Ok(flat
        .chunks_exact(2)
        .map(|pair| (pair[0], pair[1]))
        .collect())
}

fn decode_kinds(codes: &[u8]) -> Result<Vec<Kind>, RecordError> {
    codes
        .iter()
        .map(|code| Kind::from_code(*code).ok_or(RecordError::OutOfRange { field: "kind" }))
        .collect()
}

/// Checks `ends` never decrease and finish at `total`.
fn check_ends(ends: &[u32], total: usize, field: &'static str) -> Result<(), RecordError> {
    let mut previous = 0;
    for end in ends {
        if *end < previous {
            return Err(RecordError::OutOfRange { field });
        }
        previous = *end;
    }
    if previous as usize != total {
        return Err(RecordError::OutOfRange { field });
    }
    Ok(())
}

fn check_ids(ids: &[NameId], limit: usize, field: &'static str) -> Result<(), RecordError> {
    if ids.iter().any(|id| *id as usize >= limit) {
        return Err(RecordError::OutOfRange { field });
    }
    Ok(())
}

/// One file's functions as its record: the file's own sorted strings and
/// the functions over them. The file's path is not in it: the unit's key
/// names the file, so the same contents at another path is another unit.
pub(crate) fn encode_record(functions: &[Function]) -> Vec<u8> {
    let mut strings: Vec<&str> = Vec::new();
    for function in functions {
        strings.push(&function.name);
        strings.push(&function.qualified_name);
        strings.extend(function.mentions.iter().map(String::as_str));
        strings.extend(function.binds.iter().map(String::as_str));
    }
    strings.sort_unstable();
    strings.dedup();
    // Every string was just put in the table, so the search finds it.
    let id = |text: &str| end(strings.binary_search(&text).unwrap_or(0));
    let mut name = Vec::with_capacity(functions.len());
    let mut qualified = Vec::with_capacity(functions.len());
    let mut kind = Vec::with_capacity(functions.len());
    let mut lines = Vec::with_capacity(functions.len() * 2);
    let (mut mention_ends, mut mentions) = (Vec::new(), Vec::new());
    let (mut bind_ends, mut binds) = (Vec::new(), Vec::new());
    for function in functions {
        name.push(id(&function.name));
        qualified.push(id(&function.qualified_name));
        kind.push(Kind::from_name(&function.kind).code());
        lines.extend([function.lines.0, function.lines.1]);
        mentions.extend(function.mentions.iter().map(|text| id(text)));
        mention_ends.push(end(mentions.len()));
        binds.extend(function.binds.iter().map(|text| id(text)));
        bind_ends.push(end(binds.len()));
    }
    let mut writer = RecordWriter::new(RECORD_MAGIC, RECORD_VERSION);
    writer.strings(&strings);
    writer.column(&name);
    writer.column(&qualified);
    writer.bytes(&kind);
    writer.column(&lines);
    writer.column(&mention_ends);
    writer.column(&mentions);
    writer.column(&bind_ends);
    writer.column(&binds);
    writer.finish()
}

/// One file's record, read and checked.
pub(crate) struct FileRecord {
    strings: StringTable,
    name: Vec<u32>,
    qualified: Vec<u32>,
    kind: Vec<Kind>,
    lines: Vec<(u32, u32)>,
    mention_ends: Vec<u32>,
    mentions: Vec<u32>,
    bind_ends: Vec<u32>,
    binds: Vec<u32>,
}

impl FileRecord {
    /// Reads a record written by [`encode_record`].
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self, RecordError> {
        let mut reader = RecordReader::open(bytes, RECORD_MAGIC, RECORD_VERSION)?;
        let record = Self {
            strings: reader.string_table()?,
            name: reader.column()?,
            qualified: reader.column()?,
            kind: decode_kinds(reader.bytes()?)?,
            lines: unflatten_lines(&reader.column()?)?,
            mention_ends: reader.column()?,
            mentions: reader.column()?,
            bind_ends: reader.column()?,
            binds: reader.column()?,
        };
        reader.finish()?;
        record.check()?;
        Ok(record)
    }

    fn check(&self) -> Result<(), RecordError> {
        let count = self.name.len();
        let lengths = [
            self.qualified.len(),
            self.kind.len(),
            self.lines.len(),
            self.mention_ends.len(),
            self.bind_ends.len(),
        ];
        if lengths.iter().any(|length| *length != count) {
            return Err(RecordError::OutOfRange { field: "columns" });
        }
        check_ends(&self.mention_ends, self.mentions.len(), "mention ends")?;
        check_ends(&self.bind_ends, self.binds.len(), "bind ends")?;
        let limit = self.strings.len();
        check_ids(&self.name, limit, "name")?;
        check_ids(&self.qualified, limit, "qualified")?;
        check_ids(&self.mentions, limit, "mentions")?;
        check_ids(&self.binds, limit, "binds")
    }
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
    #[cfg(test)]
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

    /// Adds the functions of the file at `path`, from its record.
    pub(crate) fn add_record(&mut self, path: &str, record: &FileRecord) {
        let file = self.intern(path);
        let strings = &record.strings;
        let text = |id: u32| strings.get(id as usize).unwrap_or_default();
        let local: Vec<NameId> = (0..strings.len())
            .map(|index| self.intern(text(end(index))))
            .collect();
        let global = |id: &u32| local[*id as usize];
        for function in 0..record.name.len() {
            let name = text(record.name[function]);
            let qualified = text(record.qualified[function]);
            let container = self.container(name, qualified);
            let table = &mut self.table;
            table.file.push(file);
            table.name.push(global(&record.name[function]));
            table.qualified.push(global(&record.qualified[function]));
            table.container.push(container);
            table.kind.push(record.kind[function]);
            table.lines.push(record.lines[function]);
            let mentions = range(&record.mention_ends, &record.mentions, function);
            table.mentions.extend(mentions.iter().map(global));
            table.mention_ends.push(end(table.mentions.len()));
            let binds = range(&record.bind_ends, &record.binds, function);
            table.binds.extend(binds.iter().map(global));
            table.bind_ends.push(end(table.binds.len()));
        }
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
    fn records_merge_into_the_table_their_functions_make() {
        let first = [function("a.ts", "run", "Api.run", &["zeta", "alpha"])];
        let second = [function("b.ts", "call", "call", &["run", "alpha"])];
        let records = [encode_record(&first), encode_record(&second)];
        let mut builder = Builder::default();

        for (path, bytes) in [("a.ts", &records[0]), ("b.ts", &records[1])] {
            builder.add_record(path, &FileRecord::decode(bytes).unwrap());
        }
        let merged = builder.finish();
        let direct = Functions::from_functions(&[first[0].clone(), second[0].clone()]);

        assert_eq!(merged.names, direct.names);
        assert_eq!(texts(&merged, merged.mentions(0)), ["alpha", "zeta"]);
        assert_eq!(merged.file_text(1), "b.ts");
        assert_eq!(merged.mentions(1), direct.mentions(1));
    }

    #[test]
    fn a_segment_reads_back_as_the_table_it_was_written_from() {
        let mut top = function("a.ts", "", "", &["handler"]);
        top.kind = String::new();
        top.binds = vec!["handler".to_owned()];
        let functions =
            Functions::from_functions(&[function("a.ts", "run", "Api.run", &["go"]), top]);

        let read = Functions::decode_segment(&functions.encode_segment()).unwrap();

        assert_eq!(read.names, functions.names);
        assert_eq!(read.kind(1), Kind::TopLevel);
        assert_eq!(texts(&read, read.callable_names(1)), ["handler"]);
        assert_eq!(read.callable_names(0), functions.callable_names(0));
    }

    #[test]
    fn a_destructuring_keeps_its_kind_through_record_and_segment() {
        let mut destructuring = function("a.ts", "", "", &["make"]);
        destructuring.kind = "destructuring".to_owned();
        destructuring.binds = vec!["handler".to_owned()];
        let mut top = function("a.ts", "", "", &[]);
        top.kind = String::new();
        let mut builder = Builder::default();
        let record = encode_record(&[destructuring, top]);

        builder.add_record("a.ts", &FileRecord::decode(&record).unwrap());
        let read = Functions::decode_segment(&builder.finish().encode_segment()).unwrap();

        assert_eq!(read.kind(0).as_str(), "destructuring");
        assert_eq!(read.kind(1), Kind::TopLevel);
    }

    #[test]
    fn a_segment_with_an_id_past_its_names_is_rejected() {
        let mut functions = Functions::from_functions(&[function("a.ts", "run", "run", &[])]);
        functions.mentions.push(99);
        functions.mention_ends[0] = 1;

        let read = Functions::decode_segment(&functions.encode_segment());

        assert_eq!(
            read.err(),
            Some(RecordError::OutOfRange { field: "mentions" })
        );
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
