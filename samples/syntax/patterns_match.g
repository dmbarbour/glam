language g0

increment value = .r (value + 1)
equals expected actual = expected == actual

literalExample input = match input as
  42 => "number"
  "text" => "text"
  'ready => "atom"
  _ => "other"

listExample input = match input as
  [first] ++ ([middleHead] ++ middleTail) ++ [last] =>
    [first,middleHead,middleTail,last]
  [] => []
  _ => input

dictionaryExample input = match input as
  {selector:key,[key]:selected,optional?:optionalValue,rest} =>
    {:key,:selected,:optionalValue,:rest}
  {required:value,{nested:remainderValue}} =>
    [value,remainderValue]
  {} => {}
  {whole} => whole

tagAndTupleExample input = match input as
  ok:(left,right) => [left,right]
  (tag,[tag]:payload) => [tag,payload]
  _ => []

quotedPathExample input = match input as
  '.foo.[42] => "fixed path"
  (path,'.foo.[path]) => "computed path"
  _ => "other"

sharedViewsExample input = match input as
  ([first] ++ rest) as whole => [first,rest,whole]
  _ => []

effectfulExample input = match input as
  increment -> viewed when viewed == 42 => viewed
  equals 42 kept => kept
  value when next = value + 1 and next == 42 => value
  _ => input
