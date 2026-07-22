language g0

import 'std as std

hello = [72, 101, 108, 108, 111, 44]
world = std.list.map (\x -> x) (std.list.slice 0 (std.math.floor 6.9) "World!?")

asm.result = hello ++ [32] ++ world
