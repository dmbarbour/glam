language g0

main = do
  abstract entry
  jump = \_ -> entry
  .label "_start" -> entry
  .r (jump ())
