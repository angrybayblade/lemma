; Definitions in TypeScript and TSX. Each match captures the whole
; definition as @definition and its identifier as @name.

(function_declaration name: (identifier) @name) @definition
(generator_function_declaration name: (identifier) @name) @definition
(class_declaration name: (type_identifier) @name) @definition
(abstract_class_declaration name: (type_identifier) @name) @definition
(method_definition name: (property_identifier) @name) @definition
(interface_declaration name: (type_identifier) @name) @definition
(type_alias_declaration name: (type_identifier) @name) @definition
(enum_declaration name: (identifier) @name) @definition

; Module-level variables only; locals inside functions are not symbols.
(program (lexical_declaration (variable_declarator name: (identifier) @name) @definition))
(program (variable_declaration (variable_declarator name: (identifier) @name) @definition))
(program (export_statement declaration: (lexical_declaration (variable_declarator name: (identifier) @name) @definition)))
(program (export_statement declaration: (variable_declaration (variable_declarator name: (identifier) @name) @definition)))
