language g0

import 'std

increment value = .r (value + 1)
equals expected actual = expected == actual

forwardBinding input = list.pure do
  .r input -> [head] ++ tail
  .r [head,tail]

backwardBinding input = list.pure do
  [head] ++ tail <- .r input
  .r [head,tail]

valueBinding input = list.pure do
  [head] ++ tail = input
  .r [head,tail]

effectfulPatterns input = list.pure do
  .r input -> (increment -> viewed)
  .r viewed -> (equals viewed kept)
  .r kept

guardedPattern input = list.pure do
  .r input -> (value when value == input)
  .r value

recursivePattern input = list.pure do
  abstract head, tail
  .r input -> [head] ++ tail
  .r [head,tail]
