language g0

locals = let {; first = 1; second = 2; } in first + second
unchanged = locals where {}

object container with {;
  value = unchanged;
  object nested with {};
  extend nested with {};
}

updated = { value:container.value } with {; value := 4; }
empty_let = let {} in updated
