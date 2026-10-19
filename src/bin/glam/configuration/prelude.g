language g0
import 'std as std

# The CLI's policy, which its Rust code applies in turns on the `cli`
# machine: Rust pushes arguments, paths and texts, applies these functions,
# and pops bytes.

# The environment that a configuration starts with, and that the assembly
# sees when the configuration defines none.
empty_env = object (abstract_global_path configuration.env) with {}

# What compiling the configuration starts from.
configuration_definitions = {env:empty_env}

# The share of the environment that the CLI's root machine binds, which
# reflection reads with `.env`: the `process` the CLI runs as.
process_share process = std.object_from_dict {process:process}

# A configuration entry: the value at `path`, or `fallback` where it is
# undefined. A failure names the entry and the path looked up.
entry name path fallback value =
    std.anno context:{conf:{entry:name}} (std.default fallback (std.anno context:{eval:{op:'path_lookup, args:{path:path}}} value))

# What compiling the assembly starts from, with journal records of what the
# assembler introduces and declares before the first input.
assembly_definitions args env =
    { asm:{args:args, meta:{journal:[provided {intro:['args]}, provided {declare:['result]}]}}
    , env:env
    , meta:{journal:[provided {intro:['asm]}, provided {intro:['env]}]}
    }

provided op = {op:op, at:{asm:()}}

# The assembly's result as binary data. A failure names the result.
assembly_result assembly =
    std.anno context:{asm:{result:"asm.result"}} (std.anno 'binary assembly.asm.result)

# What a configured completion script receives.
completion_request executable =
    {executable:executable, protocol:"v0", request:"--completions"}
