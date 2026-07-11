language g0

base = object "base" with
  text = hello ++ ", " ++ target ++ "!"
  hello = "Hello"
  target = "Base"

hello = object "hello" extends base with
  target := "World"

asm.result = hello.text
