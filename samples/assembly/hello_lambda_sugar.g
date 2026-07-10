language g0

greet greeting = greeting.hello ++ ", " ++ greeting.world ++ "!"

asm.result = greet {
  , hello:"Hello"
  , world:"World"
  }
