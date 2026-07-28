language g0
import 'std

message = "Hello, World!" ++ [10]

message_section = do
  .bytes message

prepare_message = do
  .set '.message_length (list.len message)
  .get '.message_length

program = do
  .section.root 'text -> entry_cursor
  .cursor.on entry_cursor do
    # If the ELF entry ignored the published label, this byte would trap.
    .bytes [0xcc]
    .global "_start" -> _

    # Layout is entry -> exit -> message, independent of write order.
    .section.following 'text -> exit_cursor
    .section.after 'rodata exit_cursor -> message_cursor
    .cursor.label message_cursor -> message_label

    # Force the reused length before the lazy payload enters handler state.
    prepare_message -> message_length

    # Populate the final fragment first, then return to the entry fragment.
    # `seq` keeps the shared lazy payload from being stored before its reused
    # length has reached WHNF.
    seq message_length (.cursor.on message_cursor message_section)

    .mov_u32 'eax 1
    .mov_u32 'edi 1
    .mov_label_u32 'esi message_label
    .mov_u32 'edx message_length
    .syscall

    .cursor.on exit_cursor do
      .mov_u32 'eax 60
      .xor_u32 'edi 'edi
      .syscall

asm.result = env.linux_x86_64.executable program
