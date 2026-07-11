language g0

object hello with
  text = "Hello, World"

extend hello with
  text := _text ++ "!"

asm.result = hello.text
