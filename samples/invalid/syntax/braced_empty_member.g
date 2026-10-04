language g0

do_same_line = do { .r (); ; .r () }

do_across_lines = do { .r ();
  ; .r () }

do_blank_line = do { .r ();

  ; .r () }

let_across_lines = let { a = 1;
  ; b = 2 } in a

where_across_lines = a where { a = 1;
  ; b = 2 }

dict = { x:1 }
with_across_lines = dict with { x := 2;
  ; z = 3 }

match_across_lines = match 1 with { 1 => 2;
  ; _ => 3 }

object object_across_lines with { a = 1;
  ; b = 2 }
