language g0

prefix = "Hello"

object hello as h with
  target = "World"
  text = prefix ++ ", " ++ h.target ++ "!"

asm.result = hello.text
