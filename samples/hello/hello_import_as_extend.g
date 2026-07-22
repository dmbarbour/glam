language g0

import "libs/message.g" as lib

extend lib with
  message := _message ++ ", World!"

asm.result = lib.message
