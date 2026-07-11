language g0

import 'std as std

prefix = std.anno 'array "Hello, "
world = std.anno 'deque ([87, 111] ++ [114, 108, 100])
asm.result = std.anno 'binary (prefix ++ world ++ "!")
