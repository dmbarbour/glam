language g0

object named with
  hello = "Hello"

dict_parent = { target:"World" }

object hello extends named, dict_parent with
  text = hello ++ ", " ++ target ++ "!"

asm.result = hello.text
