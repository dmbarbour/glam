language g0

object base with
  text = hello ++ ", " ++ target ++ "!"
  hello = "Hello"
  target = "Base"

select_parent options = options

object hello extends select_parent base with
  target := "World"

asm.result = hello.text
