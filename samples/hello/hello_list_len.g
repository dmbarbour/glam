language g0

import 'std as std

message = "__Hello, World!__"
asm.result = std.list.slice 2 (std.list.len message - 2) message
