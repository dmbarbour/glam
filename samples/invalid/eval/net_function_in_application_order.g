language g0
import 'std

# A constant function written with the application port order: the bind's
# second port provides, so wiring data into it joins two providers.
bad = interaction_net do
    .bind -> [function, argument, result]
    .data 42 -> [value]
    .copy 0 -> [drop]
    .wire result value
    .wire argument drop
    .r function

asm.result = net_arity 0 bad
