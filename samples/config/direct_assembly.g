language g0
import 'std
import "minimal.g"

# A small, shared direct-assembly environment. Programs remain effect values
# until `linux_x86_64.executable` interprets their instruction stream.
object direct_standard_effects as effects with
  # `{}` is undefined and therefore cannot establish an overridable member.
  initial_state = {handler:{initialized:()}}

  run operation = effects.run_from effects.initial_state operation
  run_from state operation = effects.run_with effects.api state operation
  run_with effect_api state operation = (operation.eff effect_api) state

  return_effect result =
    \state -> {result:result, state:state}

  sequence_effect operation continuation =
    \state ->
      effects.continue_after
        (effects.run_from state operation)
        continuation

  continue_after first_outcome continuation =
    effects.run_from
      first_outcome.state
      (continuation first_outcome.result)

  user_get path =
    \state -> {result:state.user_state.(path), state:state}

  user_set path value =
    \state ->
      {
        result:(),
        state:
          effects.updated_state
            (['user_state] ++ path)
            value
            state
      }

  updated_state path value state =
    match path with
      [] => value
      _ => (state with { .(path) ::= \_prior -> value })

  object api with
    r result = ^effects.return_effect result
    seq operation continuation =
      ^effects.sequence_effect operation continuation
    get path = ^effects.user_get path
    set path value = ^effects.user_set path value

object direct_x86_64 as x86 extends direct_standard_effects with
  initial_state := {
    handler:{
      current_cursor:{},
      next_cursor_id:1,
      next_label_id:1,
      roots:[],
      cursors:{},
      symbols:{}
    }
  }

  extend api with
    section = {
      root:^x86.root_section,
      after:^x86.following_section,
      following:^x86.next_section
    }

    cursor = {
      on:^x86.on_cursor,
      label:^x86.label_at
    }

    mov_u32 register immediate =
      ^x86.emit (mov_u32:{register:register, immediate:immediate})

    mov_label_u32 register label_handle =
      ^x86.emit (
        mov_label_u32:{register:register, label:label_handle}
      )

    xor_u32 destination source =
      ^x86.emit (xor_u32:{destination:destination, source:source})

    label = ^x86.label_here
    publish name label_handle = ^x86.publish name label_handle
    global name = ^x86.global name
    bytes payload = ^x86.emit (bytes:payload)
    syscall = ^x86.emit (syscall:())

  cursor_handle cursor_id = cursor:cursor_id
  cursor_id cursor_handle = cursor_handle.cursor
  label_handle label_id = label:label_id

  handler_error context =
    anno assert_unit:{context:context, value:"invalid handler operation"} {}

  cursor_record kind =
    {kind:kind, content:[], following:{}}

  root_section kind =
    \state ->
      x86.allocate_root_section
        kind
        state.handler.next_cursor_id
        state

  allocate_root_section kind cursor_id state =
    {
      result:x86.cursor_handle cursor_id,
      state:
        x86.updated_state
          '.handler.next_cursor_id
          (cursor_id + 1)
          (
            x86.updated_state
              '.handler.roots
              (state.handler.roots ++ [cursor_id])
              (
                x86.updated_state
                  ['handler, 'cursors, cursor_id]
                  (x86.cursor_record kind)
                  state
              )
          )
    }

  following_section kind prior_cursor =
    \state ->
      x86.allocate_following_section
        kind
        prior_cursor
        state.handler.next_cursor_id
        state

  next_section kind =
    \state ->
      x86.allocate_following_section
        kind
        state.handler.current_cursor
        state.handler.next_cursor_id
        state

  allocate_following_section kind prior_cursor cursor_id state =
    x86.allocate_following_section_ids
      kind
      (x86.cursor_id prior_cursor)
      cursor_id
      state

  allocate_following_section_ids kind prior_cursor_id cursor_id state =
    x86.allocate_after_known_cursor
      kind
      prior_cursor_id
      cursor_id
      state.handler.cursors.[prior_cursor_id]
      state

  allocate_after_known_cursor kind prior_cursor_id cursor_id prior_cursor state =
    if prior_cursor == {} then
      x86.handler_error "cannot follow an unknown direct-assembly cursor"
    else
      x86.allocate_after_open_cursor
        kind
        prior_cursor_id
        cursor_id
        prior_cursor.following
        state

  allocate_after_open_cursor kind prior_cursor_id cursor_id following state =
    if following == {} then
      x86.allocate_unlinked_following_section
        kind
        prior_cursor_id
        cursor_id
        state
    else
      x86.handler_error "direct-assembly cursor already has a linear successor"

  allocate_unlinked_following_section kind prior_cursor_id cursor_id state =
    {
      result:x86.cursor_handle cursor_id,
      state:
        x86.updated_state
          '.handler.next_cursor_id
          (cursor_id + 1)
          (
            x86.updated_state
              ['handler, 'cursors, prior_cursor_id, 'following]
              cursor_id
              (
                x86.updated_state
                  ['handler, 'cursors, cursor_id]
                  (x86.cursor_record kind)
                  state
              )
          )
    }

  emit instruction =
    \state ->
      x86.emit_at
        state.handler.current_cursor
        instruction
        state

  emit_at cursor_handle instruction state =
    x86.emit_at_id (x86.cursor_id cursor_handle) instruction state

  emit_at_id cursor_id instruction state =
    {
      result:(),
      state:
        x86.updated_state
          ['handler, 'cursors, cursor_id, 'content]
          (
            state.handler.cursors.[cursor_id].content ++
              [instruction]
          )
          state
    }

  label_here =
    \state ->
      x86.capture_label
        state.handler.current_cursor
        state.handler.next_label_id
        state

  label_at cursor_handle =
    \state ->
      x86.capture_label
        cursor_handle
        state.handler.next_label_id
        state

  capture_label cursor_handle label_id state =
    x86.capture_label_handle
      cursor_handle
      (x86.label_handle label_id)
      label_id
      state

  capture_label_handle cursor_handle label_handle label_id state =
    {
      result:label_handle,
      state:
        x86.updated_state
          '.handler.next_label_id
          (label_id + 1)
          (
            x86.emit_at
              cursor_handle
              (label:{handle:label_handle})
              state
          ).state
    }

  publish name label_handle =
    \state ->
      if state.handler.symbols.[name] == {} then
        {
          result:(),
          state:
            x86.updated_state
              ['handler, 'symbols, name]
              label_handle
              state
        }
      else
        x86.handler_error "direct-assembly symbol is already published"

  global name =
    \state ->
      x86.publish_captured_label
        name
        (x86.label_here state)

  publish_captured_label name captured =
    x86.published_label_outcome
      captured.result
      ((x86.publish name captured.result) captured.state)

  published_label_outcome label_handle published =
    {result:label_handle, state:published.state}

  on_cursor cursor_handle operation =
    \state ->
      x86.restore_cursor
        state.handler.current_cursor
        (
          x86.run_from
            (
              x86.updated_state
                '.handler.current_cursor
                cursor_handle
                state
            )
            operation
        )

  restore_cursor cursor_handle outcome =
    {
      result:outcome.result,
      state:
        x86.updated_state
          '.handler.current_cursor
          cursor_handle
          outcome.state
    }

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
    x86.compile_layout
      code_address
      (
        x86.flatten_roots
          completed_state.handler.roots
          completed_state.handler.cursors
      )
      completed_state.handler.symbols

  compile_layout code_address instructions symbols =
    {
      code:x86.encode instructions instructions code_address,
      code_address:code_address,
      instructions:instructions,
      symbols:symbols
    }

  flatten_roots cursor_ids cursors =
    match cursor_ids with
      [] => []
      [cursor_id] ++ remaining =>
        x86.flatten_cursor cursor_id cursors ++
          x86.flatten_roots remaining cursors

  flatten_cursor cursor_id cursors =
    cursors.[cursor_id].content ++ (
      match cursors.[cursor_id].following with
        {} => []
        next_cursor_id => x86.flatten_cursor next_cursor_id cursors
    )

  label_address label_handle instructions code_address =
    code_address +
      x86.label_offset label_handle instructions 0

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
    linux.executable_compiled (^direct_x86_64.compile 0x400078 program)

  executable_compiled compiled =
    linux.executable_code
      (
        ^direct_x86_64.label_address
          compiled.symbols.["_start"]
          compiled.instructions
          compiled.code_address
      )
      compiled.code

  executable_code entry_address code_bytes =
    anno 'binary (
      linux.elf_header entry_address ++
      linux.program_header (120 + list.len code_bytes) ++
      code_bytes
    )

extend conf.env with
  x86_64 = ^direct_x86_64.api
  linux_x86_64 = ^direct_linux_x86_64
