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

initial_patterns = do
  .r 1 -> (forward)
  (backward) <- .r 2
  ((pure)) = 3
  _ <- .r 4
  .r [forward, backward, pure]

braced_patterns = do { .r 1 -> ((value)); _ = 2; .r value }
