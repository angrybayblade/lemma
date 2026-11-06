; Definitions in Go. Each match captures the whole definition as
; @definition and its identifier as @name.

(function_declaration name: (identifier) @name) @definition
(method_declaration name: (field_identifier) @name) @definition
(type_spec name: (type_identifier) @name) @definition
; Methods an interface requires, so their implementations can be found.
(method_elem name: (field_identifier) @name) @definition
(type_alias name: (type_identifier) @name) @definition
; Struct fields.
(struct_type (field_declaration_list (field_declaration name: (field_identifier) @name) @definition))

; Package-level constants and variables only.
(source_file (const_declaration (const_spec name: (identifier) @name) @definition))
(source_file (var_declaration (var_spec name: (identifier) @name) @definition))
