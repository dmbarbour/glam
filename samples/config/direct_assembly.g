language g0
import 'std
import "minimal.g"

# A small, shared direct-assembly environment. Programs remain effect values
# until `linux_x86_64.executable` interprets their instruction stream.
object direct_x86_64 as x86 with
  run operation = operation.eff x86

  r value = {result:value, instructions:[]}

  seq operation continuation =
    x86.continue_after (x86.run operation) continuation

  continue_after first_outcome continuation =
    x86.finish_sequence
      first_outcome
      (x86.run (continuation first_outcome.result))

  finish_sequence first_outcome second_outcome =
    {
      result:second_outcome.result,
      instructions:
        first_outcome.instructions ++ second_outcome.instructions
    }

  emit instruction = {result:(), instructions:[instruction]}

  mov_u32 register immediate =
    x86.emit (mov_u32:{register:register, immediate:immediate})

  mov_label_u32 register label_name =
    x86.emit (mov_label_u32:{register:register, label:label_name})

  xor_u32 destination source =
    x86.emit (xor_u32:{destination:destination, source:source})

  label label_name = x86.emit (label:{name:label_name})
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
      label:{name:_} => 0
      bytes:payload => list.len payload

  label_offset wanted instructions offset =
    match instructions with
      [instruction] ++ remaining =>
        match instruction with
          label:{name:actual} when actual == wanted => offset
          _ =>
            x86.label_offset
              wanted
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
      mov_label_u32:{register:register, label:label_name} =>
        [0xb8 + x86.register_index register] ++
          x86.little_endian
            4
            (code_address + x86.label_offset label_name all_instructions 0)
      xor_u32:{destination:destination, source:source} =>
        [
          0x31,
          0xc0 +
            (8 * x86.register_index source) +
            x86.register_index destination
        ]
      syscall:() => [0x0f, 0x05]
      label:{name:_} => []
      bytes:payload => payload

  compile code_address program =
    x86.encode instructions instructions code_address
    where instructions = (x86.run program).instructions

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
