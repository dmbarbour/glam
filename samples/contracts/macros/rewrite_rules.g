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

# This direct macro is the reader-granularity test. The recursive dispatcher
# speculatively attempts `.read.layout` wherever a fragment may continue.
layout_choose =
  .fix (\fragment_capture ->
    .r (\fragment_request ->
      match fragment_request with
        group:[fragment_open, fragment_close] => (
          .read.text fragment_open =>>
          .fix (\fragment_until ->
            .r (\_ ->
              .cut (
                .alt
                  (.read.text fragment_close =>> .r (.r ()))
                  (.alt
                    (
                      fragment_capture 'layout >>= \fragment_nested_layout ->
                      fragment_until () >>= \fragment_after_layout ->
                      .r (fragment_nested_layout =>> fragment_after_layout)
                    )
                    (
                      fragment_capture 'item >>= \fragment_item_writer ->
                      fragment_until () >>= \fragment_after_item ->
                      .r (fragment_item_writer =>> fragment_after_item)
                    )
                  )
              )
            )
          ) >>= \fragment_until ->
          fragment_until () >>= \fragment_body_writer ->
          .r (
            .write.text fragment_open =>>
            fragment_body_writer =>>
            .write.text fragment_close
          )
        )

        'layout => (
          .read.layout (
            .fix (\fragment_layout_loop ->
              .r (\_ ->
                .cut (
                  .alt
                    (.read.end =>> .r (.r ()))
                    (.alt
                      (
                        .read.anchor =>>
                        fragment_layout_loop () >>= \fragment_after_anchor ->
                        .r (.write.anchor =>> fragment_after_anchor)
                      )
                      (.alt
                        (
                          fragment_capture 'layout >>= \fragment_nested_layout ->
                          fragment_layout_loop () >>= \fragment_after_layout ->
                          .r (fragment_nested_layout =>> fragment_after_layout)
                        )
                        (
                          fragment_capture 'item >>= \fragment_layout_item ->
                          fragment_layout_loop () >>= \fragment_after_item ->
                          .r (fragment_layout_item =>> fragment_after_item)
                        )
                      )
                    )
                )
              )
            ) >>= \fragment_layout_loop ->
            fragment_layout_loop ()
          ) >>= \fragment_layout_writer ->
          .r (.write.layout fragment_layout_writer)
        )

        'item => (
          .alt
            (fragment_capture (group:["(", ")"]))
            (.alt
              (fragment_capture (group:["[", "]"]))
              (.alt
                (fragment_capture (group:["{", "}"]))
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
    )
  ) >>= \fragment_capture ->
  .read.text "(" =>>
  .alt
    (.read.text "true" =>> .r 'yes)
    (.read.text "false" =>> .r 'no) >>= \layout_selection ->
  .read.text "," =>>
  fragment_capture (group:["(", ")"]) >>= \layout_yes_writer ->
  .read.text "," =>>
  fragment_capture (group:["(", ")"]) >>= \layout_no_writer ->
  .read.text ")" =>>
  .read.end =>>
  if layout_selection == 'yes
    then layout_yes_writer
    else layout_no_writer

# This macro executes after `layout_choose` and treats the replayed layout as
# structured input. It therefore observes missing, flattened, or extra anchors.
layout_check =
  .read.sep =>>
  .read.text "(do" =>>
  .read.layout (
    .read.anchor =>>
    .read.text ".r" =>>
    .read.sep =>>
    .read.data >>= \_layout_first ->
    .read.anchor =>>
    .read.text ".r" =>>
    .read.sep =>>
    .read.data >>= \layout_second ->
    .read.end =>>
    .r layout_second
  ) >>= \layout_selected ->
  .read.text ")" =>>
  .read.end =>>
  .write.data layout_selected

@rules choose
  (true,$yes:group,$no:group)=>$yes
  (false,$yes:group,$no:group)=>$no

inline_result = @choose(false,("wrong"++("!")),("rewrite"++"-ok"))

# Expansion is right-to-left. `layout_check` receives only the logical source
# replayed by `layout_choose`, not the original source layout.
layout_result = @layout_check @layout_choose(false,(do
  .r "wrong"
  .r "still-wrong"),(do
  .r "first"
  .r "layout-ok"))

asm.result = inline_result ++ "," ++ layout_result
