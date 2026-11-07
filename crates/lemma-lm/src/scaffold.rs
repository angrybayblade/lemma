//! What `lm new behavior` writes: a checker and a behavior for a module,
//! and a checker script that already speaks the checker protocol. Pure:
//! from a resolved description of the operation under test to text.
//!
//! The script finds the code to run from its arguments, as every checker
//! must (`docs/tutorials/4-behaviors.md`), so it keeps working when the
//! operation's implementation moves. What it cannot know, the input a case
//! sends and what the replaced call answers, is marked `TODO`.

/// The operation a behavior runs, as the module names it and where its
/// code is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Code {
    /// How the module refers to it: `orders.place_order`.
    pub reference: String,
    /// Its implementation's file, relative to its repository.
    pub file: String,
    /// Its implementation's symbol: `place_order`, `Orders.place`.
    pub symbol: String,
}

/// The call a behavior replaces: the link it goes through.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Through {
    /// The graph holding the link: `links`.
    pub graph: String,
    /// The calling operation, as the module names it.
    pub caller: String,
    /// The operation called, as the module names it.
    pub provider: String,
}

/// The language a checker script is written in, from the handler's file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScriptLanguage {
    Python,
    TypeScript,
    Go,
}

impl ScriptLanguage {
    /// The script language for a handler in `file`; `None` for a file in
    /// no language the scaffold writes scripts for.
    pub(crate) fn of(file: &str) -> Option<Self> {
        let extension = file.rsplit_once('.').map(|(_, extension)| extension)?;
        match extension {
            "py" => Some(Self::Python),
            "ts" | "tsx" | "js" | "mjs" | "jsx" => Some(Self::TypeScript),
            "go" => Some(Self::Go),
            _ => None,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Python => "py",
            Self::TypeScript => "ts",
            Self::Go => "sh",
        }
    }

    fn program(self) -> &'static str {
        match self {
            Self::Python => "python3",
            Self::TypeScript => "bun",
            Self::Go => "sh",
        }
    }
}

/// Everything the scaffold needs, resolved from the module.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Scaffold {
    /// The behavior's name; the checker is named after it.
    pub name: String,
    pub handler: Code,
    pub through: Option<Through>,
    pub language: ScriptLanguage,
}

impl Scaffold {
    /// The checker's name.
    pub(crate) fn checker(&self) -> String {
        format!("{}_run", self.name)
    }

    /// Where the script goes, relative to the module's repository.
    pub(crate) fn script_path(&self) -> String {
        format!("tools/{}.{}", self.name, self.language.extension())
    }

    /// The checker and behavior to append to the module.
    pub(crate) fn module_text(&self) -> String {
        let checker = self.checker();
        let through_parameter = if self.through.is_some() {
            ",\n    through: Fact<Link>"
        } else {
            ""
        };
        let (link_lines, through_argument) = match &self.through {
            Some(through) => (
                format!(
                    "    let link = {}[{}, {}]\n    expect link.exists\n\n",
                    through.graph, through.caller, through.provider
                ),
                ", through: link",
            ),
            None => (String::new(), ""),
        };
        format!(
            "\n// Runs {handler} and reports what it returned and the calls it made;\n\
             // {script} says how.\n\
             export checker {checker}(\n    handler: Operation{through_parameter}\n): Evidence<Json> {{\n    \
             command: [\"{program}\", \"{script}\"]\n    timeout_ms: 60000\n}}\n\n\
             // TODO: say what must hold, then make it an error.\n\
             export behavior {name} {{\n    severity: warning\n\n{link_lines}    \
             let observed = need {checker}(handler: {handler}{through_argument})\n    \
             expect observed.response.status == 200\n        \
             because \"TODO: why this must hold when {handler} runs\"\n}}\n",
            handler = self.handler.reference,
            script = self.script_path(),
            program = self.language.program(),
            name = self.name,
        )
    }

    /// The checker script.
    pub(crate) fn script_text(&self) -> String {
        match self.language {
            ScriptLanguage::Python => self.python(),
            ScriptLanguage::TypeScript => self.typescript(),
            ScriptLanguage::Go => self.go(),
        }
    }

    fn python(&self) -> String {
        let replaced = match &self.through {
            Some(through) => format!(
                "# The call through the link, {caller} -> {provider}, answered here instead.\n\
                 call = arguments[\"through\"][\"values\"][\"caller\"][\"implementation\"]\n\
                 calls = []\n\n\n\
                 def reply(*args, **kwargs):\n    \
                 calls.append({{\"args\": args, \"kwargs\": kwargs}})\n    \
                 # TODO: what {provider} answers in this case.\n    \
                 return {{}}\n\n\n\
                 owner, name = attribute(call[\"file\"], call[\"symbol\"])\n\
                 setattr(owner, name, reply)\n",
                caller = through.caller,
                provider = through.provider,
            ),
            None => "calls = []\n".to_owned(),
        };
        format!(
            "{header}\
             import importlib\nimport json\nimport os\nimport sys\n\n\
             given = json.load(open(os.environ[\"LEMMA_INPUT\"]))\n\
             arguments = given[\"arguments\"]\n\
             sys.path.insert(0, given[\"root\"])\n\n\n\
             def attribute(file, symbol):\n    \
             \"\"\"The object holding `symbol` in `file`, and the symbol's last name.\"\"\"\n    \
             owner = importlib.import_module(file.removesuffix(\".py\").replace(\"/\", \".\"))\n    \
             *outer, name = symbol.split(\".\")\n    \
             for part in outer:\n        owner = getattr(owner, part)\n    \
             return owner, name\n\n\n\
             {replaced}\n\
             handler = arguments[\"handler\"][\"implementation\"]\n\
             owner, name = attribute(handler[\"file\"], handler[\"symbol\"])\n\
             # TODO: the input {handler} receives in this case. A method needs an\n\
             # instance of its class: build one and call the method on it.\n\
             request = {{}}\n\
             response = getattr(owner, name)(request)\n\n\
             with open(os.environ[\"LEMMA_OUTPUT\"], \"w\") as output:\n    \
             json.dump({{\"observed\": {{\"response\": response, \"calls\": calls}}}}, output, default=repr)\n",
            header = self.python_header(),
            handler = self.handler.reference,
        )
    }

    fn python_header(&self) -> String {
        format!(
            "\"\"\"Checker for the behavior `{name}`: runs {handler} and reports what it\n\
             returned and the calls it made.\n\n\
             Lemma writes the checker's arguments to the JSON file LEMMA_INPUT names and\n\
             runs this in the repository's root; write what you observed to the file\n\
             LEMMA_OUTPUT names, as {{\"observed\": ...}}. Each operation argument carries its\n\
             implementation's file and symbol, so this keeps working when code moves.\n\
             \"\"\"\n",
            name = self.name,
            handler = self.handler.reference,
        )
    }

    fn typescript(&self) -> String {
        let replaced = match &self.through {
            Some(through) => format!(
                "// TODO: answer the call through the link, {caller} -> {provider}, instead of\n\
                 // sending it: point its client at a fake server, or mock the module that\n\
                 // makes it (given.arguments.through.values.caller.implementation says where).\n",
                caller = through.caller,
                provider = through.provider,
            ),
            None => String::new(),
        };
        format!(
            "// Checker for the behavior `{name}`: runs {handler} and reports what it\n\
             // returned. Lemma writes the checker's arguments to the JSON file LEMMA_INPUT\n\
             // names and runs this in the repository's root; write what you observed to\n\
             // the file LEMMA_OUTPUT names, as {{ \"observed\": ... }}. Each operation\n\
             // argument carries its implementation's file and symbol.\n\
             import {{ readFileSync, writeFileSync }} from \"node:fs\";\n\
             import {{ join }} from \"node:path\";\n\n\
             const given = JSON.parse(readFileSync(process.env.LEMMA_INPUT!, \"utf8\"));\n\
             const handler = given.arguments.handler.implementation;\n\
             let target: any = await import(join(given.root, handler.file));\n\
             for (const part of handler.symbol.split(\".\")) {{\n  target = target[part];\n}}\n\n\
             {replaced}\
             // TODO: the input {handler} receives in this case.\n\
             const request = {{}};\n\
             const response = await target(request);\n\n\
             writeFileSync(process.env.LEMMA_OUTPUT!, JSON.stringify({{ observed: {{ response }} }}));\n",
            name = self.name,
            handler = self.handler.reference,
        )
    }

    fn go(&self) -> String {
        format!(
            "#!/bin/sh\n\
             # Checker for the behavior `{name}`: runs {handler} ({symbol} in {file})\n\
             # and reports what happened.\n\
             #\n\
             # Go cannot call a function by name at run time, so write a test beside the\n\
             # handler that runs it for one case and prints what it observed as JSON,\n\
             # {{\"observed\": {{\"response\": ...}}}}, then run that test here, for example:\n\
             #\n\
             #   go test ./{package} -run TestLemma -count=1 -v > \"$LEMMA_OUTPUT\"\n\
             #\n\
             # $LEMMA_INPUT holds the checker's arguments as JSON.\n\
             echo \"{script}: write the test that runs {symbol}, then run it here\" >&2\n\
             exit 1\n",
            name = self.name,
            handler = self.handler.reference,
            symbol = self.handler.symbol,
            file = self.handler.file,
            package = self
                .handler
                .file
                .rsplit_once('/')
                .map_or(".", |(directory, _)| directory),
            script = self.script_path(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orders(through: bool) -> Scaffold {
        Scaffold {
            name: "declined_charge".to_owned(),
            handler: Code {
                reference: "orders.place_order".to_owned(),
                file: "orders/handlers.py".to_owned(),
                symbol: "place_order".to_owned(),
            },
            through: through.then(|| Through {
                graph: "links".to_owned(),
                caller: "orders.charge".to_owned(),
                provider: "api.createCharge".to_owned(),
            }),
            language: ScriptLanguage::Python,
        }
    }

    #[test]
    fn the_behavior_looks_the_link_up_and_demands_evidence_through_it() {
        let text = orders(true).module_text();

        assert!(text.contains("let link = links[orders.charge, api.createCharge]"));
        assert!(
            text.contains("need declined_charge_run(handler: orders.place_order, through: link)")
        );
        assert!(text.contains("command: [\"python3\", \"tools/declined_charge.py\"]"));
    }

    #[test]
    fn without_a_link_the_checker_takes_only_the_handler() {
        let text = orders(false).module_text();

        assert!(!text.contains("through"));
        assert!(text.contains("need declined_charge_run(handler: orders.place_order)"));
    }

    #[test]
    fn the_python_script_replaces_the_linked_call_and_reports_it() {
        let script = orders(true).script_text();

        assert!(script.contains("setattr(owner, name, reply)"));
        assert!(script.contains("\"calls\": calls"));
    }

    #[test]
    fn scripts_are_chosen_by_the_handlers_language() {
        assert_eq!(
            ScriptLanguage::of("src/app.ts"),
            Some(ScriptLanguage::TypeScript)
        );
        assert_eq!(ScriptLanguage::of("cmd/main.go"), Some(ScriptLanguage::Go));
        assert_eq!(ScriptLanguage::of("README"), None);
    }
}
