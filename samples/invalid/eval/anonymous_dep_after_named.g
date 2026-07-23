language g0

import 'std

object named with
  hello = "Hello"

dict_parent = { target:"World" }

object hello extends named, object_from_dict dict_parent with
  text = hello ++ ", " ++ target ++ "!"

asm.result = hello.text
