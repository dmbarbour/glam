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

# A postfix where owns its remaining binding layout.
postfix = x + y + z where
  y = 1
  z = 2

# Leading infix lines preserve ordinary one-line precedence and associativity.
pipeline = source
  |> decode
  |> validate

# A boundary-aligned closer-only line may terminate the declaration.
terminal_group = (
  value
)
after_terminal_group = terminal_group
