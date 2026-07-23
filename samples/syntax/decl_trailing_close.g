language g0

# Declaration continuations are indented. A terminal line containing only
# closing delimiters may align with the declaration boundary.
sample_dict = {
    , a:1
    , b:2
}
sample_list = [
    , 1
    , 2
] # end sample_list 
sample_expr = (
    "Hello, world!"
)
sample_mess = [({a:(
    42
)})]
sample_continued = (
    41
    ) + 1
