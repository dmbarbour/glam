language g0

object base with
  text = hello ++ ", " ++ target ++ "!"
  hello = "Hello"
  target = "Base"

object hello extends base with
  target := "World"

asm.result = hello.text
