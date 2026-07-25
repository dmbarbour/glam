language g0

pure = if _ then "first" else "fallback"
postfix = "first" if _ else "fallback"

selected = match 1 with {
  1 => "one";
  _ => "other";
}

allResults = match* 1 with {
  1 => "one";
  _ => "other";
}

hostSearch = try_match* when {
  _ => "first";
  _ => "second";
}

hierarchical = match when {
  _ when {
    _ => "nested";
  };
}
