language g0

object base with
  text = "Hello, World"

object hello extends base with
  text := _text ++ "!"

asm.result = hello.text
