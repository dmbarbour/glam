language g0

read_pair = do first <- .read
               second <- .read
               .r [first, second]

sum =
  let first = 1
      second = 2
  first + second

w = a + b where a = 1
                b = 2

object o with a = 1
              b = 2

u = o with a := 3
           b := 4
