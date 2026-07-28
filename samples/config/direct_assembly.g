language g0
import 'std
import "minimal.g"

# A small, shared direct-assembly environment. Programs remain effect values
# until `linux_x86_64.executable` interprets their instruction stream.
object direct_standard_effects as effects with
  # `{}` is undefined and therefore cannot establish an overridable member.
  initial_state = {initialized:()}

  run operation = effects.run_from effects.initial_state operation
  run_from state operation = (operation.eff effects) state

  seq operation continuation =
    \state ->
      effects.continue_after
        (effects.run_from state operation)
        continuation

  continue_after first_outcome continuation =
    effects.run_from
      first_outcome.state
      (continuation first_outcome.result)

  r result =
    \state -> {result:result, state:state}

  get path =
    \state -> {result:state.(path), state:state}

  set path value =
    \state ->
      {
        result:(),
        state:effects.updated_state path value state
      }

  updated_state path value state =
    match path with
      [] => value
      _ => (state with { .(path) ::= \_prior -> value })

unique direct_text_cursor, direct_rodata_cursor

object direct_x86_64 as x86 extends direct_standard_effects with
  cursor = {
    text:^direct_text_cursor,
    rodata:^direct_rodata_cursor
  }

  initial_state := {
    current_cursor:^direct_text_cursor,
    cursor_order:[^direct_text_cursor, ^direct_rodata_cursor],
    streams:{
      [^direct_text_cursor]:[],
      [^direct_rodata_cursor]:[]
    }
  }

  emit instruction =
    (
      .get '.current_cursor >>= \cursor_handle ->
      .get ['streams, cursor_handle] >>= \written ->
      .set ['streams, cursor_handle] (written ++ [instruction])
    ).eff x86

  on cursor_handle operation =
    (
      .get '.current_cursor >>= \previous_cursor ->
      .set '.current_cursor cursor_handle =>>
      operation >>= \operation_result ->
      .set '.current_cursor previous_cursor =>>
      .r operation_result
    ).eff x86

  mov_u32 register immediate =
    x86.emit (mov_u32:{register:register, immediate:immediate})

  mov_label_u32 register label_handle =
    x86.emit (
      mov_label_u32:{register:register, label:label_handle}
    )

  xor_u32 destination source =
    x86.emit (xor_u32:{destination:destination, source:source})

  label label_handle = x86.emit (label:{handle:label_handle})
  bytes payload = x86.emit (bytes:payload)
  syscall = x86.emit (syscall:())

  register_index register =
    match register with
      'eax => 0
      'ecx => 1
      'edx => 2
      'ebx => 3
      'esp => 4
      'ebp => 5
      'esi => 6
      'edi => 7

  little_endian width value =
    if width == 0 then
      []
    else
      [math.mod value 256] ++
        x86.little_endian (width - 1) (math.floor (value / 256))

  instruction_size instruction =
    match instruction with
      mov_u32:{register:_, immediate:_} => 5
      mov_label_u32:{register:_, label:_} => 5
      xor_u32:{destination:_, source:_} => 2
      syscall:() => 2
      label:{handle:_} => 0
      bytes:payload => list.len payload

  label_offset wanted_label instructions offset =
    match instructions with
      [instruction] ++ remaining =>
        match instruction with
          label:{handle:actual_label} when actual_label == wanted_label => offset
          _ =>
            x86.label_offset
              wanted_label
              remaining
              (offset + x86.instruction_size instruction)

  encode instructions all_instructions code_address =
    match instructions with
      [] => []
      [instruction] ++ remaining =>
        x86.encode_instruction instruction all_instructions code_address ++
          x86.encode remaining all_instructions code_address

  encode_instruction instruction all_instructions code_address =
    match instruction with
      mov_u32:{register:register, immediate:immediate} =>
        [0xb8 + x86.register_index register] ++
          x86.little_endian 4 immediate
      mov_label_u32:{register:register, label:label_handle} =>
        [0xb8 + x86.register_index register] ++
          x86.little_endian
            4
            (
              code_address +
              x86.label_offset label_handle all_instructions 0
            )
      xor_u32:{destination:destination, source:source} =>
        [
          0x31,
          0xc0 +
            (8 * x86.register_index source) +
            x86.register_index destination
        ]
      syscall:() => [0x0f, 0x05]
      label:{handle:_} => []
      bytes:payload => payload

  compile code_address program =
    x86.compile_state code_address (x86.run program).state

  compile_state code_address completed_state =
    x86.compile_instructions
      code_address
      (
        x86.flatten_cursors
          completed_state.cursor_order
          completed_state.streams
      )

  compile_instructions code_address instructions =
    x86.encode instructions instructions code_address

  flatten_cursors cursor_handles streams =
    match cursor_handles with
      [] => []
      [cursor_handle] ++ remaining =>
        streams.[cursor_handle] ++
          x86.flatten_cursors remaining streams

object direct_linux_x86_64 as linux with
  little_endian width value =
    ^direct_x86_64.little_endian width value

  elf_header entry_address =
    [0x7f, 0x45, 0x4c, 0x46, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0] ++
    linux.little_endian 2 2 ++
    linux.little_endian 2 62 ++
    linux.little_endian 4 1 ++
    linux.little_endian 8 entry_address ++
    linux.little_endian 8 64 ++
    linux.little_endian 8 0 ++
    linux.little_endian 4 0 ++
    linux.little_endian 2 64 ++
    linux.little_endian 2 56 ++
    linux.little_endian 2 1 ++
    linux.little_endian 2 0 ++
    linux.little_endian 2 0 ++
    linux.little_endian 2 0

  program_header file_size =
    linux.little_endian 4 1 ++
    linux.little_endian 4 5 ++
    linux.little_endian 8 0 ++
    linux.little_endian 8 0x400000 ++
    linux.little_endian 8 0x400000 ++
    linux.little_endian 8 file_size ++
    linux.little_endian 8 file_size ++
    linux.little_endian 8 0x1000

  executable program =
    linux.executable_code (^direct_x86_64.compile 0x400078 program)

  executable_code code_bytes =
    anno 'binary (
      linux.elf_header 0x400078 ++
      linux.program_header (120 + list.len code_bytes) ++
      code_bytes
    )

extend conf.env with
  x86_64 = ^direct_x86_64
  linux_x86_64 = ^direct_linux_x86_64
