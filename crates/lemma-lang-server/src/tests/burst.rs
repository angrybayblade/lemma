//! A burst of edits to a large program costs one check.
//!
//! The client's messages and the server's answers pass through gates that
//! make the test independent of timing: the burst is held back until the
//! server has answered the request before it, and the server is held in
//! that answer until the whole burst has been read. Every edit is then
//! waiting when the server next looks. The test prints how long the first
//! check and the burst took; it asserts only how many checks ran.

use std::fmt::Write as _;
use std::io::{self, BufRead, Read, Write};
use std::ops::Range;
use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{notification, output, request};
use crate::{Url, read_message, serve, write_message};

/// Modules in the generated program, besides its root.
const MODULES: usize = 300;
/// Edits in the burst.
const CHANGES: usize = 50;
/// The request answered just before the burst, and the one just after.
const BEFORE_BURST: i64 = 2;
const AFTER_BURST: i64 = 3;
/// How long a gate waits before the test gives up, so a regression that
/// deadlocks fails instead of hanging.
const PATIENCE: Duration = Duration::from_secs(300);

/// A latch: closed until opened, then open for good.
#[derive(Default)]
struct Gate {
    is_open: Mutex<bool>,
    opened: Condvar,
}

impl Gate {
    fn open(&self) {
        *self.is_open.lock().unwrap() = true;
        self.opened.notify_all();
    }

    fn wait(&self) {
        let is_open = self.is_open.lock().unwrap();
        let (is_open, waited) = self
            .opened
            .wait_timeout_while(is_open, PATIENCE, |is_open| !*is_open)
            .unwrap();
        assert!(*is_open && !waited.timed_out(), "a gate never opened");
    }
}

/// The client's messages, the burst held back until the request before it
/// has been answered.
struct GatedInput {
    bytes: Vec<u8>,
    position: usize,
    burst: Range<usize>,
    answered: Arc<Gate>,
    burst_read: Arc<Gate>,
}

impl BufRead for GatedInput {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.position == self.burst.start {
            self.answered.wait();
        }
        let end = if self.position < self.burst.start {
            self.burst.start
        } else {
            self.bytes.len()
        };
        Ok(&self.bytes[self.position..end])
    }

    /// Opens `burst_read` once reading goes past the burst: by then every
    /// edit in it has been read and handed to the server.
    fn consume(&mut self, amount: usize) {
        self.position += amount;
        if self.position > self.burst.end {
            self.burst_read.open();
        }
    }
}

impl Read for GatedInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let available = self.fill_buf()?;
        let count = available.len().min(buffer.len());
        buffer[..count].copy_from_slice(&available[..count]);
        self.consume(count);
        Ok(count)
    }
}

/// The server's messages. Answering the request before the burst holds
/// the server until the burst has been read.
struct GatedOutput {
    bytes: Vec<u8>,
    flushed: usize,
    answered: Arc<Gate>,
    burst_read: Arc<Gate>,
    /// When each request was answered, by id.
    answer_times: Vec<(i64, Instant)>,
}

impl Write for GatedOutput {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    /// The server flushes after each whole message.
    fn flush(&mut self) -> io::Result<()> {
        let message = read_message(&mut &self.bytes[self.flushed..])?;
        self.flushed = self.bytes.len();
        let id = message
            .as_ref()
            .and_then(|message| message.get("id"))
            .and_then(Value::as_i64);
        let Some(id) = id else {
            return Ok(());
        };
        self.answer_times.push((id, Instant::now()));
        if id == BEFORE_BURST {
            self.answered.open();
            self.burst_read.wait();
        }
        Ok(())
    }
}

/// Writes a program of `count` modules under `root`, each importing the
/// three after it and the standard library, and a root module importing
/// them all.
fn write_program(root: &Path, count: usize) {
    std::fs::create_dir_all(root.join("mods")).unwrap();
    let mut imports = String::new();
    for number in 0..count {
        let path = root.join(format!("mods/m{number:03}.lm"));
        std::fs::write(path, module_text(number, count)).unwrap();
        writeln!(imports, "import big.mods.m{number:03}").unwrap();
    }
    let text = format!("module big\n\nimport lemma.http\n{imports}");
    std::fs::write(root.join("mod.lm"), text).unwrap();
}

fn module_text(number: usize, count: usize) -> String {
    let name = format!("{number:03}");
    let later: Vec<String> = (number + 1..count.min(number + 4))
        .map(|other| format!("{other:03}"))
        .collect();
    let mut text = format!(
        "module big.mods.m{name}\n\nimport * as http from lemma.http\nimport * as go from lemma.go\nimport {{ Link }} from lemma.links\n"
    );
    for other in &later {
        writeln!(
            text,
            "import {{ r{other}, S{other} }} from big.mods.m{other}"
        )
        .unwrap();
    }
    write!(
        text,
        "\nlet code = go.file(\"src/m{name}/client.go\")\n\nexport relation R{name}(a: Text, b: Text)\n\nexport rule r{name}(a: Text, b: Text) {{\n    (a, b) :- R{name}(a, b)\n"
    )
    .unwrap();
    if let [first, second, ..] = later.as_slice() {
        writeln!(
            text,
            "    (a, c) :-\n        r{first}(a, b),\n        r{second}(b, c)"
        )
        .unwrap();
    }
    write!(
        text,
        "    (a, b) :-\n        http.calls(a, b, _, _),\n        !R{name}(b, a)\n}}\n\nexport service S{name} {{\n    index: {{ include: [\"src/m{name}\"] }}\n\n    export operation op{name} {{\n        implementation: code.symbol(\"Op{name}\")\n    }}\n\n    client operation call{name} {{\n        implementation: code.method(\"Client\", \"Call{name}\")\n        sends: code.type(\"Request{name}\")\n    }}\n}}\n"
    )
    .unwrap();
    if let Some(next) = later.first() {
        write!(
            text,
            "\nexport graph links{name} : Link {{\n    S{name}.call{name} -> S{next}.op{next}\n}}\n"
        )
        .unwrap();
    }
    write!(
        text,
        "\nexport require req{name} {{\n    severity: warning\n    when:\n        r{name}(a, b),\n        a != b\n    message: \"{{a}} reaches {{b}}\"\n}}\n"
    )
    .unwrap();
    text
}

/// The messages framed, and where the burst's bytes are.
fn frame(before: &[Value], burst: &[Value], after: &[Value]) -> (Vec<u8>, Range<usize>) {
    let mut bytes = Vec::new();
    for message in before {
        write_message(&mut bytes, message).unwrap();
    }
    let start = bytes.len();
    for message in burst {
        write_message(&mut bytes, message).unwrap();
    }
    let end = bytes.len();
    for message in after {
        write_message(&mut bytes, message).unwrap();
    }
    (bytes, start..end)
}

/// Edits the root module `CHANGES` times; the last edit imports a module
/// that does not exist, so its check reports one problem.
fn burst(uri: &Url, text: &str) -> Vec<Value> {
    (1..=CHANGES)
        .map(|number| {
            let edit = if number == CHANGES {
                "import big.mods.missing\n".to_owned()
            } else {
                format!("// edit {number}\n")
            };
            notification(
                "textDocument/didChange",
                &json!({
                    "textDocument": { "uri": uri, "version": number + 1 },
                    "contentChanges": [{ "text": format!("{text}{edit}") }],
                }),
            )
        })
        .collect()
}

fn answered_at(output: &GatedOutput, id: i64) -> Instant {
    output
        .answer_times
        .iter()
        .find(|(answered, _)| *answered == id)
        .map(|(_, time)| *time)
        .unwrap()
}

/// The client's side of the session: open the root module, ask for its
/// symbols, edit it in a burst, ask again and exit. Returns the framed
/// messages and where the burst's bytes are.
fn client_messages(uri: &Url, text: &str) -> (Vec<u8>, Range<usize>) {
    let symbols = |id| {
        request(
            id,
            "textDocument/documentSymbol",
            &json!({ "textDocument": { "uri": uri } }),
        )
    };
    let before = [
        request(1, "initialize", &json!({ "capabilities": {} })),
        notification("initialized", &json!({})),
        notification(
            "textDocument/didOpen",
            &json!({ "textDocument": { "uri": uri, "languageId": "lemma", "version": 1, "text": text } }),
        ),
        symbols(BEFORE_BURST),
    ];
    let after = [
        symbols(AFTER_BURST),
        request(99, "shutdown", &Value::Null),
        notification("exit", &Value::Null),
    ];
    frame(&before, &burst(uri, text), &after)
}

/// An input and an output sharing the two gates.
fn gated(bytes: Vec<u8>, burst: Range<usize>) -> (GatedInput, GatedOutput) {
    let (answered, burst_read) = (Arc::new(Gate::default()), Arc::new(Gate::default()));
    let input = GatedInput {
        bytes,
        position: 0,
        burst,
        answered: Arc::clone(&answered),
        burst_read: Arc::clone(&burst_read),
    };
    let output = GatedOutput {
        bytes: Vec::new(),
        flushed: 0,
        answered,
        burst_read,
        answer_times: Vec::new(),
    };
    (input, output)
}

/// The diagnostics published for `uri`, in order.
fn published_for(messages: &[Value], uri: &Url) -> Vec<Value> {
    messages
        .iter()
        .filter(|message| {
            message["method"] == "textDocument/publishDiagnostics"
                && message["params"]["uri"] == json!(uri)
        })
        .map(|message| message["params"]["diagnostics"].clone())
        .collect()
}

#[test]
fn a_burst_of_changes_to_a_large_program_is_checked_once() {
    let program = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    write_program(program.path(), MODULES);
    let path = program.path().join("mod.lm");
    let text = std::fs::read_to_string(&path).unwrap();
    let uri = Url::from_file_path(&path).unwrap();
    let (bytes, burst) = client_messages(&uri, &text);
    let (input, mut written) = gated(bytes, burst);

    let started = Instant::now();
    serve(input, &mut written, cache.path()).unwrap();

    let opened = answered_at(&written, BEFORE_BURST);
    let finished = answered_at(&written, AFTER_BURST);
    eprintln!(
        "a program of {} modules: opening and the first check took {:?}; {CHANGES} changes took {:?}",
        MODULES + 1,
        opened - started,
        finished - opened
    );
    let published = published_for(&output(written.bytes), &uri);
    assert_eq!(
        published.len(),
        2,
        "one check for the open, one for the burst"
    );
    assert_eq!(published[0], json!([]));
    let last = published[1].as_array().unwrap();
    assert_eq!(last.len(), 1, "the check saw the last edit: {last:#?}");
}
