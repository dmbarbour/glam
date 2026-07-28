language g0
import 'std

unique message_label

message = "Hello, World!" ++ [10]

message_section = do
  .label message_label
  .bytes message

program = do
  .mov_u32 'eax 1
  .mov_u32 'edi 1
  .mov_label_u32 'esi message_label
  .mov_u32 'edx (list.len message)
  .syscall
  .on env.x86_64.cursor.rodata message_section
  .mov_u32 'eax 60
  .xor_u32 'edi 'edi
  .syscall

asm.result = env.linux_x86_64.executable program
