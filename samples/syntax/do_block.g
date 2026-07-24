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

increment input = .r (input + 1)
equals expected actual = expected == actual

effectful_patterns = do
  .r [1, 2] -> [first, second] as whole
  .r first -> (increment -> viewed)
  .r second -> (equals 2 kept)
  .r whole -> (_ when viewed < kept and .r (viewed + kept) -> total)
  .r total

backward_effectful_pattern = do
  (increment -> viewed) <- .r 2
  .r viewed

pure_effectful_pattern = do
  (increment -> viewed) = 3
  .r viewed
