language g0

import 'std

base = { hello:"Hello", target:"Base" }

object hello extends object_from_dict base with
  target := "World"
  text = hello ++ ", " ++ target ++ "!"

asm.result = hello.text
