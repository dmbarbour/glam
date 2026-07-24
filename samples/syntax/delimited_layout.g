language g0

# The opening line does not establish a delimiter group's content anchor.
# The first later line beginning a member or separator does.
dense = [1,2,3,4,
  5,6,7,8,9,10]

leading = [1,2
  ,3,4
  ,5,6]

# A deeper line continues the current member. The next member returns to the
# content anchor selected by the first next-line member.
continued = [
  make
    long_argument,
  next_member
]

tuple = (first,
  second,
  third)

dictionary = {first:1,
  second:2,
  third:3}

effect = do { .r ()
  ; .r ()
  ; .r () }
