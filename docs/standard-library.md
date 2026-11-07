# The standard library

Every module can use the standard library's modules: `import * as http from lemma.http`. This page is generated from the library itself (`cargo test -p lemma-lang` checks it; `LEMMA_BLESS=1` rewrites it). Hovering a name in an editor shows the same signatures.

## Everywhere

- `local(path: Text): Repository`: A repository's working tree, relative to the declaring file.
- `git(url: Text, rev?: Text): Repository`: A repository cloned from `url`, read at `rev` (a commit, preferably).

### Methods

- `SourceFile.symbol(name: Text): Resolution<Symbol>`: A symbol defined in the file, such as `create_charge` or `Charges.Create`.
- `SourceFile.method(type: Text, name: Text): Resolution<Symbol>`: A method of a type defined in the file.
- `SourceFile.type(name: Text): Schema`: A type defined in the file, read as a schema by its language's serializer rules.
- `Document.at(path: Text): Schema`: The schema at a JSON path in the document, such as `$.create_charge.request`.
- `Json.has(name: Text): Bool`: Whether a JSON object has the field, `null` included. Always a `Bool`.

## `lemma.http`

HTTP contracts between services, their generated clients and consumers.

### Functions

- `openapi(document: Text): Operations`: The operations an OpenAPI document in the repository describes.
- `generated_client(generated_from: List<Text>, specs?: List<Text>, call_names?: List<Text>): Client`: A client generated from OpenAPI documents, one function per operation.
- `adapter(name: Text, entry: List<Text>, directory?: Text, setup?: Command, env?: Json, exclude?: List<Text>, timeout_ms?: Int): Operations`: Operations one of Lemma's built-in adapters reads from the service's code, by its name.
- `adapter_command(command: Command, entry: List<Text>, directory?: Text, setup?: Command, env?: Json, exclude?: List<Text>, timeout_ms?: Int): Operations`: Operations a command reads from the service's code, following docs/adapters.md.
- `inferred_links(services?: List<Service>): Facts`: The links Lemma infers from calls in code to documented operations.

### Declarations

- relation `operation`: `(service: Text, operation: Text, method: Text, path: Text)`
- relation `parameter`: `(operation: Text, name: Text, location: Text)`
- relation `required_parameter`: `(operation: Text, name: Text, location: Text)`
- relation `request_field`: `(operation: Text, field: Text, leaf: Text)`
- relation `required_request_field`: `(operation: Text, field: Text, leaf: Text)`
- relation `response_field`: `(operation: Text, field: Text, leaf: Text)`
- relation `route`: `(service: Text, method: Text, path: Text, file: Text, line: Int)`
- relation `code_names_path`: `(service: Text, path: Text)`
- relation `client_operation`: `(client: Text, chain: Text, method: Text, path: Text, file: Text, line: Int)`
- relation `client_parameter`: `(client: Text, method: Text, path: Text, name: Text, location: Text, file: Text, line: Int)`
- relation `calls_path`: `(consumer: Text, method: Text, path: Text, file: Text, line: Int)`
- relation `calls_client`: `(consumer: Text, chain: Text, file: Text, line: Int)`
- relation `mentions`: `(scope: Text, name: Text)`
- relation `calls_function`: `(scope: Text, name: Text)`
- relation `in_file`: `(function: Text, file: Text)`
- relation `named`: `(function: Text, simple_name: Text)`
- relation `imports`: `(file: Text, imported: Text)`
- relation `strict_request`: `(operation: Text)`
- relation `rejects_unknown_fields`: `(service: Text)`
- relation `route_schema`: `(service: Text, method: Text, path: Text, part: Text, file: Text, schema: Text)`
- relation `inline_strict_route`: `(service: Text, method: Text, path: Text, part: Text)`
- relation `strict_schema`: `(file: Text, name: Text)`
- relation `union_member`: `(file: Text, union: Text, member: Text)`
- relation `part_of_service`: `(file: Text, service: Text)`
- relation `addressed_to`: `(path: Text, service: Text)`
- relation `public_operation`: `(operation: Text)`
- relation `public_service`: `(service: Text)`
- rule `references_operation`: `(c: Text, op: Text, f: Text, l: Int)`
- rule `own_operation`: `(f: Text, op: Text)`
- rule `calls`: `(c: Text, op: Text, f: Text, l: Int)`
- rule `local_callee`: `(c: Text, g: Text)`
- rule `imported_callee`: `(c: Text, g: Text)`
- rule `uses`: `(c: Text, n: Text)`
- rule `sends`: `(c: Text, n: Text)`
- rule `removed_operation`: `(op: Text)`
- rule `kept_operation`: `(op: Text)`
- rule `removed_parameter`: `(op: Text, n: Text, loc: Text)`
- rule `new_required_parameter`: `(op: Text, n: Text, loc: Text)`
- rule `removed_request_field`: `(op: Text, fld: Text, leaf: Text)`
- rule `new_required_request_field`: `(op: Text, fld: Text, leaf: Text)`
- rule `removed_response_field`: `(op: Text, fld: Text, leaf: Text)`
- rule `strict_in_scope`: `(f: Text, n: Text)`
- rule `loose_union`: `(f: Text, u: Text)`
- rule `strict_definition`: `(f: Text, n: Text)`
- rule `strict_route`: `(s: Text, m: Text, p: Text, part: Text)`
- rule `rejects_unknown`: `(op: Text)`
- rule `rejects_unknown_parameter`: `(op: Text)`
- rule `served`: `(m: Text, p: Text)`
- rule `served_path`: `(p: Text)`
- rule `called_before`: `(c: Text, m: Text, p: Text)`
- rule `unserved_call`: `(c: Text, m: Text, p: Text, f: Text, l: Int)`
- rule `specified`: `(s: Text, m: Text, p: Text)`
- rule `routed`: `(s: Text, m: Text, p: Text)`
- rule `specified_route`: `(s: Text, m: Text, f: Text, l: Int)`
- rule `served_before`: `(s: Text, m: Text, p: Text)`
- rule `added_operation`: `(s: Text, op: Text, m: Text, p: Text)`
- rule `routed_before`: `(s: Text, m: Text, p: Text)`
- rule `added_route`: `(s: Text, m: Text, p: Text, f: Text, l: Int)`
- rule `reads_routes`: `(s: Text)`
- rule `calls_at_head`: `(c: Text, op: Text, f: Text, l: Int)`
- rule `called_at_head`: `(op: Text)`
- rule `unrouted_operation`: `(op: Text)`
- requirement `consumer_calls_unserved_operation`: `requirement`
- requirement `consumer_calls_removed_operation`: `requirement`
- requirement `consumer_misses_new_required_parameter`: `requirement`
- requirement `consumer_misses_new_required_field`: `requirement`
- requirement `consumer_sends_removed_parameter`: `requirement`
- requirement `consumer_sends_ignored_parameter`: `requirement`
- requirement `consumer_sends_rejected_field`: `requirement`
- requirement `consumer_uses_removed_request_field`: `requirement`
- requirement `consumer_uses_removed_response_field`: `requirement`
- requirement `consumer_calls_changed_operation`: `requirement`
- requirement `client_declares_removed_parameter`: `requirement`
- requirement `client_calls_removed_operation`: `requirement`
- requirement `consumer_calls_unrouted_operation`: `requirement`
- requirement `operation_without_route`: `requirement`
- requirement `route_without_operation`: `requirement`
- requirement `operation_without_consumer`: `requirement`
- requirement `route_not_in_spec`: `requirement`

## `lemma.links`

Links between operations: a caller's client operation and the provider's operation it calls. `lemma check` checks each link for persistence (both operations exist and their code resolves) and for schema agreement.

### Declarations

- relation `Link`: `(caller: ClientOperation, provider: Operation)`

## `lemma.python`

Resolution requests into Python code: `file(path)` names a source file, and its `symbol`, `method` and `type` name what is in it. They are resolved against each snapshot a check reads.

### Functions

- `file(path: Text): SourceFile`: A source file, relative to the repository root.
- `module(name: Text): SourceFile`: A Python module by its dotted name, read from the file its dots name.


## `lemma.typescript`

Resolution requests into Typescript code: `file(path)` names a source file, and its `symbol`, `method` and `type` name what is in it. They are resolved against each snapshot a check reads.

### Functions

- `file(path: Text): SourceFile`: A source file, relative to the repository root.


## `lemma.go`

Resolution requests into Go code: `file(path)` names a source file, and its `symbol`, `method` and `type` name what is in it. They are resolved against each snapshot a check reads.

### Functions

- `file(path: Text): SourceFile`: A source file, relative to the repository root.


## `lemma.rust`

Resolution requests into Rust code: `file(path)` names a source file, and its `symbol`, `method` and `type` name what is in it. They are resolved against each snapshot a check reads.

### Functions

- `file(path: Text): SourceFile`: A source file, relative to the repository root.


## `lemma.json`

Schemas inside JSON documents: a committed file, `document(file)`, or a command's output, `command(...)`, each read with `.at("$.json.path")`.

### Functions

- `document(file: Text, format?: Text): Document`: A JSON document in the repository; `format: "openapi"` converts OpenAPI 3.0's `nullable`.
- `command(command: Command, output?: Text, env?: Json, cache?: Bool, format?: Text): Document`: A command's JSON output, produced at each snapshot and kept by the code's Git tree.


## `lemma.hono`

Reads a service's operations from its code with Lemma's built-in `hono` adapter: `app(entry: [...])`. See docs/adapters.md.

### Functions

- `app(entry: List<Text>, directory?: Text, setup?: Command, env?: Json, exclude?: List<Text>, timeout_ms?: Int): Operations`: Operations a Hono app built with @hono/zod-openapi registers.


## `lemma.fastapi`

Reads a service's operations from its code with Lemma's built-in `fastapi` adapter: `app(entry: [...])`. See docs/adapters.md.

### Functions

- `app(entry: List<Text>, directory?: Text, setup?: Command, env?: Json, exclude?: List<Text>, timeout_ms?: Int): Operations`: Operations a FastAPI app registers.

