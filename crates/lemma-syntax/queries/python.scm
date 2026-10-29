; Definitions in Python. Each match captures the whole definition as
; @definition and its identifier as @name.

(function_definition name: (identifier) @name) @definition
(class_definition name: (identifier) @name) @definition

; Module-level assignments only; locals inside functions are not symbols.
(module (expression_statement (assignment left: (identifier) @name) @definition))
