language g0

asm.result = (\greeting -> greeting.hello ++ ", " ++ greeting.world ++ "!") {
  , hello:"Hello"
  , world:"World"
  }
