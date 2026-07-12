language g0

base = { hello:"Hello", target:"Base" }

object hello extends base with
  target := "World"
  text = hello ++ ", " ++ target ++ "!"

asm.result = hello.text
