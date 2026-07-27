language g0
import 'std

meta.macro.env = {}

# This rule compiler exercises balanced inline fragments and generates a later
# macro as embedded data.
rules =
  (\fragment_group ->
    (\fragment_item ->
      (\rules_choose ->
        .read.sep =>>
        .read.regex "[a-z][A-Za-z0-9_]*" >>= \rules_name ->
        .read.layout (
          .read.anchor =>>
          .case "the true rewrite rule" (
            .read.text "(true,$yes:group,$no:group)=>$yes"
          ) =>>
          .read.anchor =>>
          .case "the false rewrite rule" (
            .read.text "(false,$yes:group,$no:group)=>$no"
          ) =>>
          .read.end
        ) =>>
        .write.text rules_name.span =>>
        .write.sep =>>
        .write.text "=" =>>
        .write.sep =>>
        .write.data rules_choose
      ) (
        .fix (\fragment_until ->
          .r (\fragment_close ->
            .alt
              (.read.text fragment_close =>> .r (.r ()))
              (
                fragment_item fragment_until >>= \fragment_item_writer ->
                fragment_until fragment_close >>= \fragment_rest_writer ->
                .r (fragment_item_writer =>> fragment_rest_writer)
              )
          )
        ) >>= \fragment_until ->
        .read.text "(" =>>
        .alt
          (.read.text "true" =>> .r 'yes)
          (.read.text "false" =>> .r 'no) >>= \rules_selection ->
        .read.text "," =>>
        fragment_group fragment_until "(" ")" >>= \rules_yes_writer ->
        .read.text "," =>>
        fragment_group fragment_until "(" ")" >>= \rules_no_writer ->
        .read.text ")" =>>
        .read.end =>>
        if rules_selection == 'yes
          then rules_yes_writer
          else rules_no_writer
      )
    ) (
      \fragment_until ->
        .alt
          (fragment_group fragment_until "(" ")")
          (.alt
            (fragment_group fragment_until "[" "]")
            (.alt
              (fragment_group fragment_until "{" "}")
              (.alt
                (.read.data >>= \fragment_data ->
                  .r (.write.data fragment_data))
                (.alt
                  (.read.sep =>> .r (.write.sep))
                  (.read.text_span >>= \fragment_span ->
                    .r (.write.text fragment_span.span))
                )
              )
            )
          )
    )
  ) (
    \fragment_until fragment_open fragment_close ->
      .read.text fragment_open =>>
      fragment_until fragment_close >>= \fragment_body_writer ->
      .r (
        .write.text fragment_open =>>
        fragment_body_writer =>>
        .write.text fragment_close
      )
  )

# Try layout first, then rewind and accept an inline option. The bounded helper
# keeps this protocol sample focused on speculative layout and anchor replay.
layout_choose =
  (\read_option ->
    .read.sep =>>
    .alt
      (.read.text "true" =>> .r 'yes)
      (.read.text "false" =>> .r 'no) >>= \layout_selection ->
    .read.sep =>>
    read_option () >>= \layout_yes_writer ->
    read_option () >>= \layout_no_writer ->
    .read.end =>>
    if layout_selection == 'yes
      then layout_yes_writer
      else layout_no_writer
  ) (
    \_ ->
      .cut (
        .alt
          (.case "a two-item layout option" (
            .read.layout (
              .case "the first layout anchor" (.read.anchor) =>>
              .case "the first layout value" (.read.data) >>= \layout_first ->
              .case "the second layout anchor" (.read.anchor) =>>
              .case "the second layout value" (.read.data) >>= \layout_second ->
              .case "the end of the option layout" (.read.end) =>>
              .r {first:layout_first, second:layout_second}
            ) >>= \layout_values ->
            .r (
              .write.text "(do" =>>
              .write.layout (
                .write.anchor =>>
                .write.text ".r" =>>
                .write.sep =>>
                .write.data layout_values.first =>>
                .write.anchor =>>
                .write.text ".r" =>>
                .write.sep =>>
                .write.data layout_values.second
              ) =>>
              .write.text ")"
            )
          ))
          (.case "an inline option" (
            .read.text "(" =>>
            .read.data >>= \inline_value ->
            .read.text ")" =>>
            .r (
              .write.text "(do" =>>
              .write.sep =>>
              .write.text ".r" =>>
              .write.sep =>>
              .write.data inline_value =>>
              .write.text ")"
            )
          ))
      )
  )

# This macro executes after `layout_choose` and treats the replayed layout as
# structured input. It therefore observes missing, flattened, or extra anchors.
layout_check =
  .case "separation before the replayed option" (.read.sep) =>>
  .case "the replayed do header" (.read.text "(do") =>>
  .read.layout (
    .case "the replayed first anchor" (.read.anchor) =>>
    .case "the replayed first return" (.read.text ".r") =>>
    .case "separation before the replayed first value" (.read.sep) =>>
    .case "the replayed first value" (.read.data) >>= \_layout_first ->
    .case "the replayed second anchor" (.read.anchor) =>>
    .case "the replayed second return" (.read.text ".r") =>>
    .case "separation before the replayed second value" (.read.sep) =>>
    .case "the replayed second value" (.read.data) >>= \layout_second ->
    .case "the end of the replayed layout" (.read.end) =>>
    .r layout_second
  ) >>= \layout_selected ->
  .case "the dedent before the replayed do close" (.read.sep) =>>
  .case "the replayed do close" (.read.text ")") =>>
  .case "the end of the replayed option" (.read.end) =>>
  .write.data layout_selected

@rules choose
  (true,$yes:group,$no:group)=>$yes
  (false,$yes:group,$no:group)=>$no

inline_result = @choose(false,("wrong"++("!")),("rewrite"++"-ok"))

# Expansion is right-to-left. `layout_check` receives only the logical source
# replayed by `layout_choose`, not the original source layout.
layout_result = @layout_check @layout_choose false ("wrong")
  "first"
  "layout-ok"

asm.result = inline_result ++ "," ++ layout_result
