language g0

prefix = "Hello"

object hello with
  target = "World"
  text = ^prefix ++ ", " ++ target ++ "!"

asm.result = hello.text
