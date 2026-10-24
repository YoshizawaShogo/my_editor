; Makefile highlighting, vendored rather than taken from tree-sitter-make: its
; own query paints every recipe line as a string and names its captures for
; another editor's palette (`@conditional`, `@include`, `@repeat`), which this
; one has no colours for. The capture names here are the ones
; render::highlight_color knows. Where two patterns could cover the same text,
; a predicate keeps them apart instead of relying on match order.

(comment) @comment

; Directives: make's own control flow and bookkeeping.
[
  "ifeq" "ifneq" "ifdef" "ifndef" "else" "endif"
  "define" "endef"
  "include" "sinclude" "-include"
  "export" "unexport" "override" "private" "vpath" "undefine"
] @keyword

; Assignment flavours — `=` and `:=` differ in when the value is expanded, so
; they are worth seeing.
[
  "=" ":=" "::=" "?=" "+=" "!="
] @operator

; Built-in functions: `$(wildcard …)`, `$(patsubst …)`, `$(shell …)`.
(function_call function: _ @function)
(shell_function function: _ @function)

(string) @string

; Variables, where they are set and where they are read.
(variable_assignment name: (word) @property)
(define_directive name: (word) @property)
(variable_reference (word) @property)

; `$@`, `$<`, `$^`: set by make for each rule, not by the file.
(automatic_variable) @macro

; Rule targets are what a Makefile is read by, so they stand out like function
; definitions. `.PHONY` and its kin are directives in the shape of a rule.
((targets (word) @function)
  (#not-match? @function "^\\."))
((targets (word) @keyword)
  (#match? @keyword "^\\."))
