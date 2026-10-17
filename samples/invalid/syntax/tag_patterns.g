language g0

# A tag builds data, so it is never a predicate or a view.
joint v = match v as
  a:Nat n => n
constructor v = match v as
  (a:) p => p
viewed v = match v as
  ((a:) -> p) => p
