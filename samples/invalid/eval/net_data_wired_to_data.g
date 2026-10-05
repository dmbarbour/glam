language g0
import 'std

bad = interaction_net do
    .data 1 -> [left]
    .data 2 -> [right]
    .wire left right
    .data 3 -> [result]
    .r result

asm.result = net_arity 0 bad
