language g0

message = :greeting
world = ["target"]:"World"
punctuation_tag = "punctuation"
punctuation = :[punctuation_tag]
asm.result = (message "Hello").greeting ++ ", " ++ world.["target"] ++ (punctuation "!").["punctuation"]
