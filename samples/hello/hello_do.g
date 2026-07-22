language g0
import 'std

hello = .r "Hello"
world = .r "World"

message = do
  greeting <- hello
  world -> target
  punctuation = "!"
  .r (greeting ++ ", " ++ target ++ punctuation)

asm.result = list.head (list.pure message)
