language g0

import 'std

left = { hello:"Hello" }
right = { target:"World" }

object hello extends object_from_dict left, object_from_dict right with
  text = hello ++ ", " ++ target ++ "!"

asm.result = hello.text
