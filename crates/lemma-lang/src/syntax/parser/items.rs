//! Top-level items and the declarations that hold fields: modules,
//! imports, workspaces, services, operations, graphs and the rest.

use super::Parser;
use crate::syntax::ast::{
    AllowDeclaration, BehaviorDeclaration, CheckDeclaration, CheckMember, CheckerDeclaration,
    Declaration, Declared, EntityDeclaration, Field, File, FunctionDeclaration, GraphBody,
    GraphDeclaration, GraphRow, Import, ImportName, ImportSelection, Item, ModuleName,
    OperationDeclaration, Reexport, RelationDeclaration, Repo, RequireDeclaration, RunSelection,
    ServiceDeclaration, ServiceMember, TypeDeclaration, TypedField, Workspace,
};
use crate::syntax::lexer::TokenKind;

fn is_closing(kind: TokenKind) -> bool {
    matches!(
        kind,
        TokenKind::RightBrace | TokenKind::RightParen | TokenKind::RightBracket
    )
}

impl Parser<'_> {
    pub(super) fn file(&mut self) -> File {
        self.skip_separators(false);
        let module = self.module_line();
        let items = self.separated(TokenKind::End, false, Parser::item);
        File {
            module,
            items,
            comments: Vec::new(),
        }
    }

    fn module_line(&mut self) -> Option<ModuleName> {
        if self.eat_word("module").is_none() {
            self.error_here("expected `module <name>` first");
            return None;
        }
        let name = self.module_name("the module's name");
        if !self.at_separator(false) && !self.at(TokenKind::End) {
            self.error_here("expected a new line after the module's name");
            self.recover_in_block(self.position);
        }
        name
    }

    /// `a.b.c`, stopping before a `.*`.
    pub(super) fn module_name(&mut self, what: &str) -> Option<ModuleName> {
        let first = self.name(what)?;
        let start = first.span.start;
        let mut segments = vec![first];
        while self.at(TokenKind::Dot)
            && matches!(self.nth_kind(1), TokenKind::Name | TokenKind::EscapedName)
        {
            self.bump();
            segments.push(self.name("a name after `.`")?);
        }
        Some(ModuleName {
            segments,
            span: self.span_from(start),
        })
    }

    fn item(&mut self) -> Option<Item> {
        if self.at_word("import") {
            return self.import().map(Item::Import);
        }
        if self.at_word("export") && self.nth_kind(1) == TokenKind::LeftBrace {
            return self.reexport().map(Item::Reexport);
        }
        if self.at_word("workspace") && self.nth_kind(1) == TokenKind::LeftBrace {
            return Some(Item::Workspace(self.workspace()));
        }
        if self.at_word("repo") && self.nth_kind(1) == TokenKind::Name {
            return self.repo().map(Item::Repo);
        }
        self.declared().map(Item::Declaration)
    }

    fn import(&mut self) -> Option<Import> {
        let start = self.bump().span.start;
        let selection = if self.at(TokenKind::LeftBrace) {
            let names = self.import_names();
            self.expect_word("from")?;
            ImportSelection::Names(names)
        } else if self.eat(TokenKind::Star).is_some() {
            self.expect_word("as")?;
            let alias = self.name("a name for the module")?;
            self.expect_word("from")?;
            ImportSelection::Namespace(alias)
        } else {
            ImportSelection::Module
        };
        let module = self.module_name("a module name")?;
        Some(Import {
            selection,
            module,
            span: self.span_from(start),
        })
    }

    fn import_names(&mut self) -> Vec<ImportName> {
        self.delimited(
            TokenKind::LeftBrace,
            TokenKind::RightBrace,
            false,
            |parser| parser.comma_separated(TokenKind::RightBrace, Parser::import_name),
        )
    }

    fn import_name(&mut self) -> Option<ImportName> {
        let name = self.name("a name to import")?;
        let alias = if self.eat_word("as").is_some() {
            Some(self.name("a new name")?)
        } else {
            None
        };
        Some(ImportName { name, alias })
    }

    fn reexport(&mut self) -> Option<Reexport> {
        let start = self.bump().span.start;
        let names = self.import_names();
        self.expect_word("from")?;
        let module = self.module_name("a module name")?;
        Some(Reexport {
            names,
            module,
            span: self.span_from(start),
        })
    }

    pub(super) fn expect_word(&mut self, word: &str) -> Option<()> {
        if self.eat_word(word).is_some() {
            return Some(());
        }
        self.error_here(&format!("expected `{word}`"));
        None
    }

    fn workspace(&mut self) -> Workspace {
        let start = self.bump().span.start;
        let members = self.delimited(
            TokenKind::LeftBrace,
            TokenKind::RightBrace,
            true,
            |parser| parser.separated(TokenKind::RightBrace, false, Parser::workspace_member),
        );
        let mut repos = Vec::new();
        let mut fields = Vec::new();
        for member in members {
            match member {
                Ok(repo) => repos.push(repo),
                Err(field) => fields.push(field),
            }
        }
        Workspace {
            repos,
            fields,
            span: self.span_from(start),
        }
    }

    /// A `repo` declaration, or else a field.
    fn workspace_member(&mut self) -> Option<Result<Repo, Field>> {
        if self.at_word("repo") && self.nth_kind(1) == TokenKind::Name {
            return self.repo().map(Ok);
        }
        self.field().map(Err)
    }

    fn repo(&mut self) -> Option<Repo> {
        let start = self.bump().span.start;
        let name = self.name("the repository's name")?;
        self.expect(TokenKind::Equals, "`=`")?;
        self.skip_newlines();
        let source = self.expression();
        Some(Repo {
            name,
            source,
            span: self.span_from(start),
        })
    }

    fn declared(&mut self) -> Option<Declared> {
        let start = self.peek().span.start;
        let is_exported = self.eat_word("export").is_some();
        let declaration = self.declaration()?;
        Some(Declared {
            is_exported,
            declaration,
            span: self.span_from(start),
        })
    }

    fn declaration(&mut self) -> Option<Declaration> {
        let word = if self.at(TokenKind::Name) {
            self.text(self.peek()).to_owned()
        } else {
            String::new()
        };
        match word.as_str() {
            "let" => self.binding().map(Declaration::Binding),
            "type" => self.type_declaration().map(Declaration::Type),
            "entity" => self.entity().map(Declaration::Entity),
            "relation" => self.relation().map(Declaration::Relation),
            "service" => self.service().map(Declaration::Service),
            "graph" => self.graph().map(Declaration::Graph),
            "rule" => self.rule().map(Declaration::Rule),
            "require" => self.require().map(Declaration::Require),
            "allow" => self.allow().map(Declaration::Allow),
            "behavior" => self.behavior().map(Declaration::Behavior),
            "fn" => self.function().map(Declaration::Function),
            "checker" => self.checker().map(Declaration::Checker),
            "check" => self.check().map(Declaration::Check),
            _ => {
                self.error_here("expected a declaration such as `let`, `service`, `relation`, `rule` or `require`");
                None
            }
        }
    }

    fn type_declaration(&mut self) -> Option<TypeDeclaration> {
        let start = self.bump().span.start;
        let name = self.name("the type's name")?;
        self.expect(TokenKind::Equals, "`=`")?;
        self.skip_newlines();
        let value = self.type_expression()?;
        Some(TypeDeclaration {
            name,
            value,
            span: self.span_from(start),
        })
    }

    fn entity(&mut self) -> Option<EntityDeclaration> {
        let start = self.bump().span.start;
        let name = self.name("the entity's name")?;
        let fields = self.typed_field_block();
        Some(EntityDeclaration {
            name,
            fields,
            span: self.span_from(start),
        })
    }

    /// `{ name: Type ... }`, fields separated by newlines, `;` or commas.
    pub(super) fn typed_field_block(&mut self) -> Vec<TypedField> {
        self.delimited(
            TokenKind::LeftBrace,
            TokenKind::RightBrace,
            true,
            |parser| parser.separated(TokenKind::RightBrace, true, Parser::typed_field),
        )
    }

    fn relation(&mut self) -> Option<RelationDeclaration> {
        let start = self.bump().span.start;
        let name = self.name("the relation's name")?;
        let columns = self.parameters();
        Some(RelationDeclaration {
            name,
            columns,
            span: self.span_from(start),
        })
    }

    fn service(&mut self) -> Option<ServiceDeclaration> {
        let start = self.bump().span.start;
        let name = self.name("the service's name")?;
        let members = self.delimited(
            TokenKind::LeftBrace,
            TokenKind::RightBrace,
            true,
            |parser| parser.separated(TokenKind::RightBrace, false, Parser::service_member),
        );
        Some(ServiceDeclaration {
            name,
            members,
            span: self.span_from(start),
        })
    }

    fn service_member(&mut self) -> Option<ServiceMember> {
        let exported_offset = usize::from(self.at_word("export"));
        let word = self.nth_text(exported_offset).to_owned();
        let next = self.nth_kind(exported_offset + 1);
        let starts_member = match word.as_str() {
            "service" | "operation" => next == TokenKind::Name || next == TokenKind::EscapedName,
            "client" => self.nth_text(exported_offset + 1) == "operation",
            _ => false,
        };
        if !starts_member {
            if self.at_word("let") {
                return self.binding().map(ServiceMember::Binding);
            }
            return self.field().map(ServiceMember::Field);
        }
        let is_exported = self.eat_word("export").is_some();
        if word == "service" {
            let service = self.service()?;
            return Some(ServiceMember::Service {
                is_exported,
                service,
            });
        }
        let operation = self.operation()?;
        Some(ServiceMember::Operation {
            is_exported,
            operation,
        })
    }

    fn operation(&mut self) -> Option<OperationDeclaration> {
        let start = self.peek().span.start;
        let is_client = self.eat_word("client").is_some();
        self.expect_word("operation")?;
        let name = self.name("the operation's name")?;
        let fields = self.field_block();
        Some(OperationDeclaration {
            is_client,
            name,
            fields,
            span: self.span_from(start),
        })
    }

    /// `{ name: value ... }`.
    pub(super) fn field_block(&mut self) -> Vec<Field> {
        self.delimited(
            TokenKind::LeftBrace,
            TokenKind::RightBrace,
            true,
            |parser| parser.separated(TokenKind::RightBrace, true, Parser::field),
        )
    }

    fn graph(&mut self) -> Option<GraphDeclaration> {
        let start = self.bump().span.start;
        let name = self.name("the graph's name")?;
        self.expect(TokenKind::Colon, "`:` and the graph's relation")?;
        let relation = self.type_expression()?;
        let body = if self.eat(TokenKind::Equals).is_some() {
            self.skip_newlines();
            GraphBody::Provider(self.expression())
        } else {
            GraphBody::Rows(self.delimited(
                TokenKind::LeftBrace,
                TokenKind::RightBrace,
                true,
                |parser| parser.separated(TokenKind::RightBrace, false, |p| Some(p.graph_row())),
            ))
        };
        Some(GraphDeclaration {
            name,
            relation,
            body,
            span: self.span_from(start),
        })
    }

    fn graph_row(&mut self) -> GraphRow {
        let start = self.peek().span.start;
        if self.at(TokenKind::LeftParen) {
            let values = self.delimited(TokenKind::LeftParen, TokenKind::RightParen, false, |p| {
                p.expressions_until(TokenKind::RightParen)
            });
            return GraphRow {
                values,
                span: self.span_from(start),
            };
        }
        let first = self.expression();
        let mut values = vec![first];
        if self.eat(TokenKind::Arrow).is_some() {
            self.skip_newlines();
            values.push(self.expression());
        }
        GraphRow {
            values,
            span: self.span_from(start),
        }
    }

    fn require(&mut self) -> Option<RequireDeclaration> {
        let start = self.bump().span.start;
        let name = self.name("the requirement's name")?;
        let (when, fields) = self.fields_with_body();
        Some(RequireDeclaration {
            name,
            when,
            fields,
            span: self.span_from(start),
        })
    }

    fn allow(&mut self) -> Option<AllowDeclaration> {
        let start = self.bump().span.start;
        let target = self.module_name("the requirement it allows")?;
        let (when, fields) = self.fields_with_body();
        Some(AllowDeclaration {
            target,
            when,
            fields,
            span: self.span_from(start),
        })
    }

    fn behavior(&mut self) -> Option<BehaviorDeclaration> {
        let start = self.bump().span.start;
        let name = self.name("the behavior's name")?;
        let parameters = self.at(TokenKind::LeftParen).then(|| self.parameters());
        let body = self.block();
        Some(BehaviorDeclaration {
            name,
            parameters,
            body,
            span: self.span_from(start),
        })
    }

    fn function(&mut self) -> Option<FunctionDeclaration> {
        let start = self.bump().span.start;
        let name = self.name("the function's name")?;
        let parameters = self.parameters();
        self.expect(TokenKind::Colon, "`:` and the type it returns")?;
        let returns = self.type_expression()?;
        let body = self.block();
        Some(FunctionDeclaration {
            name,
            parameters,
            returns,
            body,
            span: self.span_from(start),
        })
    }

    fn checker(&mut self) -> Option<CheckerDeclaration> {
        let start = self.bump().span.start;
        let name = self.name("the checker's name")?;
        let parameters = self.parameters();
        self.expect(TokenKind::Colon, "`:` and the evidence it produces")?;
        let returns = self.type_expression()?;
        let fields = self.field_block();
        Some(CheckerDeclaration {
            name,
            parameters,
            returns,
            fields,
            span: self.span_from(start),
        })
    }

    fn check(&mut self) -> Option<CheckDeclaration> {
        let start = self.bump().span.start;
        let name = self.name("the check's name")?;
        let members = self.delimited(
            TokenKind::LeftBrace,
            TokenKind::RightBrace,
            true,
            |parser| parser.separated(TokenKind::RightBrace, false, Parser::check_member),
        );
        Some(CheckDeclaration {
            name,
            members,
            span: self.span_from(start),
        })
    }

    fn check_member(&mut self) -> Option<CheckMember> {
        if self.eat_word("skip").is_some() {
            return self
                .module_name("the requirement to skip")
                .map(CheckMember::Skip);
        }
        if !self.at_word("run") {
            self.error_here("expected `run` or `skip`");
            return None;
        }
        let start = self.bump().span.start;
        let target = self.module_name("a requirement or module")?;
        let is_wildcard = self.at(TokenKind::Dot) && self.nth_kind(1) == TokenKind::Star;
        if is_wildcard {
            self.bump();
            self.bump();
        }
        let arguments = self.at(TokenKind::LeftParen).then(|| self.call_arguments());
        let overrides = if self.at(TokenKind::LeftBrace) {
            self.field_block()
        } else {
            Vec::new()
        };
        Some(CheckMember::Run(RunSelection {
            target,
            is_wildcard,
            arguments,
            overrides,
            span: self.span_from(start),
        }))
    }

    /// `name: value`.
    pub(super) fn field(&mut self) -> Option<Field> {
        let name = self.name("a field name")?;
        self.expect(TokenKind::Colon, "`:` after the field's name")?;
        self.skip_newlines();
        let value = self.expression();
        let span = name.span.to(value.span);
        Some(Field { name, value, span })
    }

    /// `name: Type`.
    pub(super) fn typed_field(&mut self) -> Option<TypedField> {
        let name = self.name("a name")?;
        self.expect(TokenKind::Colon, "`:` and a type")?;
        self.skip_newlines();
        let field_type = self.type_expression()?;
        let span = name.span.to(field_type.span());
        Some(TypedField {
            name,
            field_type,
            span,
        })
    }

    /// `(name: Type, ...)`.
    pub(super) fn parameters(&mut self) -> Vec<TypedField> {
        self.delimited(
            TokenKind::LeftParen,
            TokenKind::RightParen,
            false,
            |parser| parser.comma_separated(TokenKind::RightParen, Parser::typed_field),
        )
    }

    /// Items separated by commas until `close`, a trailing comma allowed.
    pub(super) fn comma_separated<T>(
        &mut self,
        close: TokenKind,
        mut item: impl FnMut(&mut Self) -> Option<T>,
    ) -> Vec<T> {
        let mut items = Vec::new();
        while !self.at(close) && !self.at(TokenKind::End) {
            let Some(value) = item(self) else {
                self.recover_before(close);
                break;
            };
            items.push(value);
            if self.eat(TokenKind::Comma).is_none() {
                break;
            }
        }
        items
    }

    /// Skips to just before `close`, or before any unmatched closing
    /// bracket.
    fn recover_before(&mut self, close: TokenKind) {
        let mut depth = 0usize;
        loop {
            let kind = self.peek_kind();
            let closes = is_closing(kind);
            if kind == TokenKind::End || (depth == 0 && (kind == close || closes)) {
                return;
            }
            depth = super::adjust_depth(depth, kind);
            self.bump();
        }
    }
}
