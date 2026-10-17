language g0

prefix = "Hello"

object hello with
  alias h
  target = "World"
  text = prefix ++ ", " ++ h.target ++ "!"

asm.result = hello.text
