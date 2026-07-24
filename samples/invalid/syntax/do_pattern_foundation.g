language g0

missing_backward = do
  <- .r 1
  .r ()

missing_forward = do
  .r 1 ->
  .r ()

missing_value = do
  = 1
  .r ()

unsupported = do
  [first] ++ rest ++ another <- .r [1]
  .r first

reserved = do
  .r 1 -> binary
  .r ()

missing_backward_operation = do { value <-; .r () }

missing_value_operation = do { value =; .r () }

missing_forward_operation = do { -> value; .r () }

final_binding = do
  .r 1 -> value

refutable_as = do
  .r [1, 2] -> 1 as item
  .r ()
