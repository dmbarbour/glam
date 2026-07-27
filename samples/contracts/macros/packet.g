language g0
import 'std

meta.macro.env = {}

packet = object _ as packet_object with
  encode _packet_layout packet_input =
    [list.at 0 packet_input] ++
    [math.floor (list.at 1 packet_input / 256)] ++
    [math.mod (list.at 1 packet_input) 256] ++
    list.at 2 packet_input ++
    [list.at 3 packet_input] ++
    list.at 4 packet_input

  decode _packet_layout packet_binary =
    let { packet_length = (list.at 1 packet_binary * 256) + list.at 2 packet_binary } in [
      list.at 0 packet_binary,
      packet_length,
      list.slice 3 (3 + packet_length) packet_binary,
      list.at (3 + packet_length) packet_binary,
      list.slice (4 + packet_length) (list.len packet_binary) packet_binary
    ]

  read_field =
    .case "a packet field: field Name Type [LengthField]" (
      .read.text "field" =>>
      .read.sep =>>
      .read.regex "[a-z][A-Za-z0-9_]*" >>= \packet_field_name ->
      .read.sep =>>
      .case "a packet field type: u8, u16, u32, bytes, or utf8" (
        .alt
          (.read.text "u8" =>> .r "u8")
          (.alt
            (.read.text "u16" =>> .r "u16")
            (.alt
              (.read.text "u32" =>> .r "u32")
              (.alt
                (.read.text "bytes" =>> .r "bytes")
                (.read.text "utf8" =>> .r "utf8")
              )
            )
          )
      ) >>= \packet_field_type ->
      .alt
        (
          .r (field:{
            name:packet_field_name.span,
            type:packet_field_type,
            dependency:{}
          })
        )
        (
          .read.sep =>>
          .read.regex "[a-z][A-Za-z0-9_]*" >>= \packet_dependency ->
          .r (field:{
            name:packet_field_name.span,
            type:packet_field_type,
            dependency:packet_dependency.span
          })
        )
    )

  read_fields _ =
    .alt
      (.read.end =>> .r [])
      (
        .read.anchor =>>
        packet_object.read_field >>= \packet_field ->
        packet_object.read_fields () >>= \packet_more_fields ->
        .r ([packet_field] ++ packet_more_fields)
      )

  read_variants _ =
    .alt
      (.read.end =>> .r [])
      (
        .read.anchor =>>
        .case "a numeric packet choice followed by an indented field body" (
          .read.data >>= \packet_variant_tag ->
          .read.layout (packet_object.read_fields ()) >>= \packet_variant_fields ->
          .r {
            tag:packet_variant_tag,
            fields:packet_variant_fields
          }
        ) >>= \packet_variant ->
        packet_object.read_variants () >>= \packet_more_variants ->
        .r ([packet_variant] ++ packet_more_variants)
      )

  read_items _ =
    .alt
      (.read.end =>> .r [])
      (
        .read.anchor =>>
        .alt
          (
            .case "a packet byte order: endian big" (
              .read.text "endian" =>>
              .read.sep =>>
              .read.text "big" =>>
              .r (endian:"big")
            )
          )
          (
            .alt
              packet_object.read_field
              (
                .case "a packet choice: choice Name with numeric variants" (
                  .read.text "choice" =>>
                  .read.sep =>>
                  .read.regex "[a-z][A-Za-z0-9_]*" >>= \packet_choice_name ->
                  .read.layout (packet_object.read_variants ()) >>= \packet_variants ->
                  .r (choice:{
                    name:packet_choice_name.span,
                    variants:packet_variants
                  })
                )
              )
          ) >>= \packet_item ->
        packet_object.read_items () >>= \packet_more_items ->
        .r ([packet_item] ++ packet_more_items)
      )

  expand =
    .read.sep =>>
    .read.regex "[a-z][A-Za-z0-9_]*" >>= \packet_name ->
    .read.layout (
      packet_object.read_items () >>= \packet_layout ->
      .write.text "object" =>>
      .write.sep =>>
      .write.text packet_name.span =>>
      .write.sep =>>
      .write.text "with" =>>
      .write.layout (
        .write.anchor =>>
        .write.text "codec" =>>
        .write.sep =>>
        .write.text "=" =>>
        .write.sep =>>
        .write.data packet_layout =>>
        .write.anchor =>>
        .write.text "encode" =>>
        .write.sep =>>
        .write.text "=" =>>
        .write.sep =>>
        .write.data packet_object.encode =>>
        .write.sep =>>
        .write.data packet_layout =>>
        .write.anchor =>>
        .write.text "decode" =>>
        .write.sep =>>
        .write.text "=" =>>
        .write.sep =>>
        .write.data packet_object.decode =>>
        .write.sep =>>
        .write.data packet_layout
      )
    )

@packet.expand message
  endian big
  field version u8
  field length u16
  field payload bytes length

  choice kind
    1
      field request_id u32
    2
      field message utf8 length

example_packet = [1, 2, "Hi", 2, "OK"]
encoded = message.encode example_packet
decoded = message.decode encoded
decoded_payload = list.at 2 decoded
decoded_variant = list.at 4 decoded
asm.result = encoded
