language g0

import "libs/message.g" as lib

object hello extends lib with
  message := _message ++ ", World!"

asm.result = hello.message
