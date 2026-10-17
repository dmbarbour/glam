language g0

hello = object "hello" with
  alias _h
  target = "World"
  text = "Hello, " ++ h.target ++ "!"

asm.result = hello.text
