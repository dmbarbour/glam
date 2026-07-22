language g0

d = {
  , ['greet]:"Hello, "
  , ['hello]:{
    , [1]:{
      , [2]:{
        , [3]:"World"
        }
      }
    }
  , ['punct]:"!"
  }
asm.result = d.['greet] ++ d.['hello].([1,2] ++ [3]) ++ d.['punct]
