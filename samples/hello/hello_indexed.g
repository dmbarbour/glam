language g0

d = {
  , hello:"Hello"
  , world:{ [42]:"World" }
  , ['punct]:"!"
}
idx = 42
asm.result = d.['hello] ++ ", " ++ d.['world].[idx] ++ d.['punct]
