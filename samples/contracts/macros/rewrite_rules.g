language g0
import 'std

meta.macro.env = {}

rules =
  # A captured fragment is represented by an effect that writes it. The value
  # is reusable, so a rule may reorder, duplicate, or discard a metavariable
  # without exposing compiler syntax objects.
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

@rules choose
  (true,$yes:group,$no:group)=>$yes
  (false,$yes:group,$no:group)=>$no

# The unselected branch is deliberately nested more deeply. Capturing and
# replaying the selected group must preserve its balanced structure.
asm.result = @choose(false,("wrong"++("!")),("rewrite"++"-ok"))
