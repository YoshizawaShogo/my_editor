; Highlights for Tcl. Vendored here because the bca-tree-sitter-tcl crate ships
; no highlights query of its own. Capture names match the substrings that
; render::highlight_color keys on (comment/string/number/keyword/operator/function).

(comment) @comment

(number) @number
(quoted_word) @string
(escaped_character) @string.escape

; Control-flow and builtin command keywords. These are the literal tokens inside
; the grammar's dedicated nodes (set/proc/if/while/...), so they never double up
; with the generic `command` rule below.
[
  "set"
  "proc"
  "if"
  "elseif"
  "else"
  "while"
  "foreach"
  "namespace"
  "try"
  "catch"
  "finally"
  "expr"
  "global"
  "regexp"
  "error"
] @keyword

; Word operators inside expr.
[
  "eq"
  "ne"
  "in"
  "ni"
] @keyword.operator

; Symbolic operators inside expr.
[
  "=="
  "!="
  "<="
  ">="
  "<"
  ">"
  "+"
  "-"
  "*"
  "/"
  "%"
  "**"
  "&&"
  "||"
  "!"
  "&"
  "|"
  "^"
  "~"
  "<<"
  ">>"
  "?"
  ":"
] @operator

; $name, ${name}, $arr(idx)
(variable_substitution) @variable

; proc definitions and ordinary command invocations.
(procedure
  name: (_) @function)

(command
  name: (simple_word) @function.call)
