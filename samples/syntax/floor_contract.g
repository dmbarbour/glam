language g0

# An inline right-hand side and a next-line right-hand side use the same
# declaration floor.
inline_lambda = \value ->
  finish value

next_line_lambda =
  \value ->
  finish value

# A layout do is already accepted as the final application argument.
final_do = begin_op_header do
  Operation1 -> r1
  Operation2 -> r2
  finish r1 r2

# An unparenthesized lambda may be the final application argument or the tail
# operand of an infix expression. Its body has ordinary rightward extent.
mapped = map values \value -> transform value |> finish

bound = Operation1 >>= \r1 ->
  Operation2 r1 >>= \r2 ->
  finish r1 r2

# A postfix where owns its remaining binding layout.
postfix = x + y + z where
  y = 1
  z = 2

# A layout body yields at its first dedent. The inner with remains inside the
# where binding because its member anchor is deeper than the binding anchor.
nested_binding = op3a where
    op3a = op3 with
        C = value

# One dedent may close several nested bodies before where resumes the nearest
# compatible enclosing expression.
resumed_where = outer with
    member = inner with
        value = replacement
  where
    replacement = value

# Leading infix lines preserve ordinary one-line precedence and associativity.
pipeline = source
  |> decode
  |> validate

# A leading operator at the established anchor may close a layout do and
# resume the enclosing chain.
processed = source
  |> process do
    input <- .read
    .r (transform input)
  |> finish

# The same resumption closes a with body. A later where at that anchor wraps
# the complete recovered operator chain.
configured = source
  |> configure with
    A := 42
    B := derive A
  |> finish
  where
    derive = transform

# A boundary-aligned closer-only line may terminate the declaration.
terminal_group = (
  value
)
after_terminal_group = terminal_group
