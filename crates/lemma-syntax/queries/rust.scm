; Definitions in Rust. Each match captures the whole definition as
; @definition and its identifier as @name.

(function_item name: (identifier) @name) @definition
; Methods a trait requires, so their implementations can be found.
(function_signature_item name: (identifier) @name) @definition
(struct_item name: (type_identifier) @name) @definition
(enum_item name: (type_identifier) @name) @definition
(union_item name: (type_identifier) @name) @definition
(trait_item name: (type_identifier) @name) @definition
(type_item name: (type_identifier) @name) @definition
(const_item name: (identifier) @name) @definition
(static_item name: (identifier) @name) @definition
(macro_definition name: (identifier) @name) @definition
; Struct fields.
(struct_item body: (field_declaration_list (field_declaration name: (field_identifier) @name) @definition))
