language g0
import 'std

# A net's exposed port must provide a value; an application's argument port
# consumes one.
bad = interaction_net do
    .bind -> [call, argument, result]
    .data 1 -> [value]
    .copy 0 -> [drop]
    .wire call value
    .wire result drop
    .r argument

asm.result = net_arity 0 bad
