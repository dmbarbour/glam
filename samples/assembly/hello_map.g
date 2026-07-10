language g0

import 'std as std

hello = [72, 101, 108, 108, 111, 44, 32]
world = std.list.map (\x -> x) "World!"

asm.result = hello ++ world
