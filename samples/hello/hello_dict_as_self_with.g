language g0

suffix = "!"
base = { text:"Hello, World" }
hello = base with
  alias self
  text := _text ++ ^suffix

asm.result = hello.text
