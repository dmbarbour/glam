language g0

suffix = "!"
base = { text:"Hello, World" }
hello = base with
  alias h
  text := _h.text ++ suffix
  result = h.text

asm.result = hello.result
