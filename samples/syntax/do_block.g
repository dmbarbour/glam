language g0

main = do
  .global "_start" -> declaration
  symbol = "_start"
  .label symbol
  .r declaration

singleton = do .r 1

braced = do { value <- .r 1; .r value }

empty = do {}

nested = consume [do { .r 1 }, do {; .r 2; }]
