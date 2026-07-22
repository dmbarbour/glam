language g0

prefix = "Hello"
separator = ", "

object hello with
  target = "World"
  text = ^(prefix ++ separator) ++ target ++ "!"

asm.result = hello.text
