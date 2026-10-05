language g0
import 'std

# Every constructed node must be reachable from the exposed port.
bad = interaction_net do
    .data 1 -> [kept]
    .data 2 -> [lost]
    .copy 0 -> [drop]
    .wire lost drop
    .r kept

asm.result = net_arity 0 bad
