language g0

hello = object "hello" as _h with
  target = "World"
  text = "Hello, " ++ h.target ++ "!"

asm.result = hello.text
