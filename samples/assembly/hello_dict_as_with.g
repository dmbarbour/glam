language g0

suffix = "!"
base = { text:"Hello, World" }
hello = base as h with
  text := _h.text ++ suffix
  result = h.text

asm.result = hello.result
