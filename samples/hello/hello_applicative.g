language g0
import 'std

hello = .r "Hello"
world = .r "World"
format = .r (\world hello -> hello ++ ", " ++ world ++ "!")

asm.result = list.head (list.pure (hello !> world !> format))
