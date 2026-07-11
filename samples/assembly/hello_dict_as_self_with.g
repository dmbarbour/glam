language g0

suffix = "!"
base = { text:"Hello, World" }
hello = base as self with
  text := _text ++ ^suffix

asm.result = hello.text
