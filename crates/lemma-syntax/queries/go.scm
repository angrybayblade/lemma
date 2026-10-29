; Definitions in Go. Each match captures the whole definition as
; @definition and its identifier as @name.

(function_declaration name: (identifier) @name) @definition
(method_declaration name: (field_identifier) @name) @definition
(type_spec name: (type_identifier) @name) @definition
(type_alias name: (type_identifier) @name) @definition

; Package-level constants and variables only.
(source_file (const_declaration (const_spec name: (identifier) @name) @definition))
(source_file (var_declaration (var_spec name: (identifier) @name) @definition))
