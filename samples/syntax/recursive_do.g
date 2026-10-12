language g0

main = do
  declare entry
  jump = \_ -> entry
  .label "_start" -> entry
  .r (jump ())
