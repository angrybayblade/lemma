; The outline panel and breadcrumbs in Zed: every declaration, with its
; keyword as context. Bindings are listed at the top level only.

(service_declaration
  "service" @context
  name: (identifier) @name) @item

(operation_declaration
  "operation" @context
  name: (identifier) @name) @item

(entity_declaration
  "entity" @context
  name: (identifier) @name) @item

(relation_declaration
  "relation" @context
  name: (identifier) @name) @item

(type_declaration
  "type" @context
  name: (identifier) @name) @item

(graph_declaration
  "graph" @context
  name: (identifier) @name) @item

(rule_declaration
  "rule" @context
  name: (identifier) @name) @item

(require_declaration
  "require" @context
  name: (identifier) @name) @item

(allow_declaration
  "allow" @context
  requirement: (qualified_name) @name) @item

(behavior_declaration
  "behavior" @context
  name: (identifier) @name) @item

(function_declaration
  "fn" @context
  name: (identifier) @name) @item

(checker_declaration
  "checker" @context
  name: (identifier) @name) @item

(check_declaration
  "check" @context
  name: (identifier) @name) @item

(workspace_declaration
  "workspace" @name) @item

(source_file
  (let_declaration
    "let" @context
    name: (identifier) @name) @item)
