; Bracket pairs for Zed, for matching and rainbow brackets.

("(" @open ")" @close)

("[" @open "]" @close)

("{" @open "}" @close)

(type_arguments
  "<" @open
  ">" @close)

((string
  "\"" @open
  "\"" @close)
  (#set! rainbow.exclude))
