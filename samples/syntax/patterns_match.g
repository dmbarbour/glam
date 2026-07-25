language g0

increment value = .r (value + 1)
equals expected actual = expected == actual

literalExample input = match input with
  42 => "number"
  "text" => "text"
  'ready => "atom"
  _ => "other"

listExample input = match input with
  [first] ++ ([middleHead] ++ middleTail) ++ [last] =>
    [first,middleHead,middleTail,last]
  [] => []
  _ => input

dictionaryExample input = match input with
  {selector:key,[key]:selected,optional?:optionalValue,rest} =>
    {:key,:selected,:optionalValue,:rest}
  {required:value,{nested:remainderValue}} =>
    [value,remainderValue]
  {} => {}
  {whole} => whole

tagAndTupleExample input = match input with
  ok:(left,right) => [left,right]
  (tag,[tag]:payload) => [tag,payload]
  _ => []

quotedPathExample input = match input with
  '.foo.[42] => "fixed path"
  (path,'.foo.[path]) => "computed path"
  _ => "other"

sharedViewsExample input = match input with
  ([first] ++ rest) as whole => [first,rest,whole]
  _ => []

effectfulExample input = match input with
  increment -> viewed when viewed == 42 => viewed
  equals 42 kept => kept
  value when next = value + 1 and next == 42 => value
  _ => input
