language g0

left = { hello:"Hello" }
right = { target:"World" }

object hello extends left, right with
  text = hello ++ ", " ++ target ++ "!"

asm.result = hello.text
