language g0

hello who = "Hello, " ++ who
hello who ::= \prior -> prior who ++ "!"

asm.result = hello "World"
