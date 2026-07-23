language g0
import 'std

hello = .r "Hello"
world = .r "World"
format = .r (\world_text hello_text -> hello_text ++ ", " ++ world_text ++ "!")

asm.result = list.head (list.pure (hello !> world !> format))
