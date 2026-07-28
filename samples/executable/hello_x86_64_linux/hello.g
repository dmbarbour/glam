language g0
import 'std

message = "Hello, World!" ++ [10]

program = do
  .mov_u32 'eax 1
  .mov_u32 'edi 1
  .mov_label_u32 'esi 'message
  .mov_u32 'edx (list.len message)
  .syscall
  .mov_u32 'eax 60
  .xor_u32 'edi 'edi
  .syscall
  .label 'message
  .bytes message

asm.result = env.linux_x86_64.executable program
