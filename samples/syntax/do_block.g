language g0

main = do
  .global "_start" -> declaration
  symbol = "_start"
  .label symbol
  .r declaration
