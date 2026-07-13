language g0

# bad let: indentation of names should align vertically
bad_let1 =
  let hello = "Hello"
  world = "World"
  hello ++ ", " ++ world ++ "!"

# bad let: 'in' does not appear in multi-line form
bad_let2 =
  let hello = "Hello"
      world = "World"
  in hello ++ ", " ++ world ++ "!"

# bad where: indentation of names should align vertically
bad_where1 = hello ++ ", " ++ world ++ "!" where
    hello = "Hello"
  world = "World"

asm.result = bad_where1
