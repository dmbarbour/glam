language g0

import 'std as std

empty = (,)
message = (, "Hello, World!",)
asm.result = std.list.head message.tuple
