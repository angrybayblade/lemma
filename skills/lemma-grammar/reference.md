# Grammar reference

## Input relations

`lemma verify` writes these input relations, in both snapshots unless noted. A grammar receives the ones it declares and ignores the rest. Operations are named `METHOD /path` with path parameters as `{}`; consumers are `repository:file::Function` (or `repository:file` outside functions); files are `repository:file`.

| Relation | Columns | From |
| --- | --- | --- |
| `operation` | service, operation, method, path | service specs |
| `parameter` | operation, name, location | service specs |
| `required_parameter` | operation, name, location | service specs |
| `request_field` | operation, field path (`hooks[].handler.action`), last name | service specs |
| `required_request_field` | operation, field path, last name | service specs: required in every enclosing object and every union branch |
| `response_field` | operation, field path, last name | service specs, 2xx responses |
| `strict_request` | operation | service specs: `additionalProperties: false` |
| `route` | service, method, path, file, line | service route code (head only) |
| `route_schema` | service, method, path, part (`body`/`query`), file, schema | service route code (head only) |
| `inline_strict_route` | service, method, path, part | a route's own schema is `.strict()` (head only) |
| `strict_schema` | file, name | `const name = z.object(..).strict()` (head only) |
| `union_member` | file, union, member | `z.union`/`z.discriminatedUnion` members; `*` for a non-strict inline member (head only) |
| `client_operation` | client, chain, method, path, file, line | client tables, or `generated_from` specs (chain is the function) |
| `client_parameter` | client, method, path, name, location | client specs |
| `calls_path` | consumer, method (`*` if unknown), path, file, line | consumer code: path strings and request calls |
| `calls_client` | consumer, chain, file, line | consumer code: calls through client receivers and imported client functions |
| `mentions` | scope, name | names used as fields: object keys, members, keyword arguments, dictionary keys, struct fields, JSON tags |
| `in_file` | scope, file | which file a consumer or function is in |
| `imports` | file, imported file | relative and alias imports; for Go, files of one package |
| `path_family` | path, family (`/api/v3.1`) | every path in specs and calls |
| `rejects_unknown_fields` | service | manifest `rejects_unknown_fields: true` |

## Derived relations in the built-in grammar

These come from the rules in `grammars/http-contracts.yaml`; any can be printed with `lemma verify <manifest> --relation <name>` and used in new requirements.

| Relation | Columns | Holds when |
| --- | --- | --- |
| `calls` | consumer, operation, file, line | A consumer calls an operation of the base snapshot, by path (with or without a method) or through a client chain |
| `file_mentions` | file, name | A file mentions a name anywhere |
| `uses` | consumer, name | The name appears in the consumer's function, or outside functions in its file or a file it imports |
| `names_anywhere` | file, name | The name appears anywhere in the file or a file it imports |
| `removed_operation` | operation | In the base snapshot, not the head |
| `kept_operation` | operation | In both snapshots |
| `removed_parameter` | operation, name, location | A kept operation lost a parameter |
| `new_required_parameter` | operation, name, location | A kept operation now requires a parameter |
| `removed_request_field` | operation, field, leaf | A kept operation's request lost a field |
| `new_required_request_field` | operation, field, leaf | A kept operation's request now requires a field |
| `removed_response_field` | operation, field, leaf | A kept operation's response lost a field |
| `strict_route` | service, method, path, part | A route's body or query schema rejects unknown keys (zod `.strict()`) |
| `rejects_unknown` | operation | Requests sending an unknown field fail, after the change |
| `rejects_unknown_parameter` | operation | Requests sending an unknown query parameter fail, after the change |
| `unserved_call` | consumer, method, path, file, line | A changed consumer adds a call to a path no service serves, in a family a service does serve |
| `specified`, `routed`, `specified_route` | service, method, path (file, line) | Spec against code |

Helper relations used by these: `called_before`, `loose_union`, `served`, `served_family`, `served_path`, `strict_definition`, `strict_in_scope`.

## Requirement variables

A requirement's variables, in the order they first appear in a positive atom of `when`, become the finding's values (`--json` shows them by name) and fill `{name}` placeholders in `explain`.
