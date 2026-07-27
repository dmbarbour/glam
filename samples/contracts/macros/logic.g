language g0
import 'std

meta.macro.env = {}

meta.logic.facts logic_items =
  match logic_items with
    [] => []
    [fact:{arguments:[logic_left, logic_right], _}] ++ logic_rest =>
      [[logic_left, logic_right]] ++ meta.logic.facts logic_rest
    [_] ++ logic_rest =>
      meta.logic.facts logic_rest

meta.logic.query logic_items =
  match logic_items with
    [query:{from:logic_from, _}] ++ _ =>
      logic_from
    [_] ++ logic_rest =>
      meta.logic.query logic_rest

meta.logic.descendants logic_facts logic_current =
  match logic_facts with
    [] => []
    [[logic_parent, logic_child]] ++ _ when logic_parent == logic_current =>
      [logic_child] ++ meta.logic.descendants logic_facts logic_child
    [_] ++ logic_rest =>
      meta.logic.descendants logic_rest logic_current

meta.logic.run logic_items =
  meta.logic.descendants
    (meta.logic.facts logic_items)
    (meta.logic.query logic_items)

logic =
  .fix (\logic_read_goals ->
    .r (
      .alt
        (.read.end =>> .r [])
        (
          .read.anchor =>>
          .case "a logic goal: predicate Argument Argument" (
            .read.regex "[a-z][a-z_]*" >>= \logic_goal_predicate ->
            .read.sep =>>
            .read.regex "[A-Z][A-Za-z0-9_]*" >>= \logic_goal_left ->
            .read.sep =>>
            .read.regex "[A-Z][A-Za-z0-9_]*" >>= \logic_goal_right ->
            .r {
              predicate:logic_goal_predicate.span,
              arguments:[logic_goal_left.span, logic_goal_right.span]
            }
          ) >>= \logic_goal ->
          logic_read_goals >>= \logic_more_goals ->
          .r ([logic_goal] ++ logic_more_goals)
        )
    )
  ) >>= \logic_read_goals ->
  .fix (\logic_read_items ->
    .r (
      .alt
        (.read.end =>> .r [])
        (
          .read.anchor =>>
          .alt
            (
              .case "a logic fact: fact predicate Data Data" (
                .read.text "fact" =>>
                .read.sep =>>
                .read.regex "[a-z][a-z_]*" >>= \logic_fact_predicate ->
                .read.sep =>>
                .read.data >>= \logic_fact_left ->
                .read.sep =>>
                .read.data >>= \logic_fact_right ->
                .r (fact:{
                  predicate:logic_fact_predicate.span,
                  arguments:[logic_fact_left, logic_fact_right]
                })
              )
            )
            (
              .alt
                (
                  .case "a logic rule with an indented goal body" (
                    .read.text "rule" =>>
                    .read.sep =>>
                    .read.regex "[a-z][a-z_]*" >>= \logic_rule_predicate ->
                    .read.sep =>>
                    .read.regex "[A-Z][A-Za-z0-9_]*" >>= \logic_rule_left ->
                    .read.sep =>>
                    .read.regex "[A-Z][A-Za-z0-9_]*" >>= \logic_rule_right ->
                    .read.layout logic_read_goals >>= \logic_rule_goals ->
                    .r (rule:{
                      predicate:logic_rule_predicate.span,
                      parameters:[logic_rule_left.span, logic_rule_right.span],
                      goals:logic_rule_goals
                    })
                  )
                )
                (
                  .case "a logic query: query predicate Data Variable" (
                    .read.text "query" =>>
                    .read.sep =>>
                    .read.regex "[a-z][a-z_]*" >>= \logic_query_predicate ->
                    .read.sep =>>
                    .read.data >>= \logic_query_from ->
                    .read.sep =>>
                    .read.regex "[A-Z][A-Za-z0-9_]*" >>= \logic_query_into ->
                    .r (query:{
                      predicate:logic_query_predicate.span,
                      from:logic_query_from,
                      into:logic_query_into.span
                    })
                  )
                )
            ) >>= \logic_item ->
          logic_read_items >>= \logic_more_items ->
          .r ([logic_item] ++ logic_more_items)
        )
    )
  ) >>= \logic_read_items ->
  .read.layout (
    logic_read_items >>= \logic_items ->
    .write.text "meta.logic.run" =>>
    .write.sep =>>
    .write.data logic_items
  )

family = @logic
  fact parent "alice" "bob"
  fact parent "bob" "carol"

  rule ancestor X Y
    parent X Y

  rule ancestor X Y
    parent X Z
    ancestor Z Y

  query ancestor "alice" Who

asm.result =
  list.at 0 family ++ "," ++ list.at 1 family
