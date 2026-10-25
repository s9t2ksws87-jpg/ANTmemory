# Using ANTmemory 2.0

[日本語](MEMORY_USAGE.ja.md) · [Installation and routing](../README.md)

## Retrieve before working

At task start, call `search_context` for relevant decisions, constraints, and lessons. If the first query misses, try two or three different terms. A hit is prior recorded context, not confirmation that its claim remains true.

```text
search_context(query="deployment decision", top_k=6, token_budget=1200)
read_context(id="<returned id>")
```

The default search covers active hot/warm memories and returns metadata plus short summaries. Request `include_history=true`, `include_hidden=true`, or `full_content=true` explicitly as needed. Result count uses `top_k` only; `max_results` is obsolete. Token budgets are estimates and truncation is reported. `relative_score` is normalized among candidates, not a probability of relevance.

`read_context` returns Raw with status, visibility, current-version state, created/updated timestamps, and source handle when available. `list_context` supports inventory. Inspect these fields and re-observe the source before relying on consequential claims.

## Save what the next agent needs

Store decisions and reasons, stable preferences, failure lessons, environment quirks, and important task checkpoints that another agent would otherwise rediscover. Avoid secrets, transient chatter, and facts easily reconstructed from maintained files.

```text
write_context(
  topic="Deployment requires a reviewed artifact",
  content="Decision and reason, observed scope, and remaining uncertainty.",
  tags=["deployment", "decision"],
  importance="high",
  source_ref="commit:<commit-id>"
)
```

`source_ref` is an optional handle, not an automatically fetched or verified source. Use a file, commit, experiment, conversation, or URL that later agents can inspect. Add `ttl_days` only for temporary information. The writer comes from `ANT_MEMORY_CLIENT`; do not supply `agent` or `actor` to write calls. The `agent` search filter still exists.

## Revise without deleting

| Situation | Operation |
|---|---|
| Small correction | `correct_context(id, changed fields..., reason)` |
| Replace overall policy/content | `supersede_context(old_id, topic, content, metadata..., reason)` |
| Withdraw a wrong record | `retract_context(id, reason)` |
| Change importance | `change_importance(id, importance, reason)` |
| Change hot/warm/cold/archive visibility | `set_visibility(id, visibility, reason)` |
| Restore retracted/expired record without a successor | `restore_context(id, reason)` |
| Inspect events and relations | `get_context_history(id)` |

Corrections create a new revision and inherit omitted topic, body, tags, importance, source handle, and the original absolute expiry. Set `clear_ttl=true` to remove expiry; do not combine it with `ttl_days`. Replacement metadata should be supplied deliberately; `supersede_context` is not the metadata-inheritance shortcut.

Superseded revisions with successors cannot be restored as current. Never delete and recreate a record to simulate an update. No physical deletion tool is exposed. Visibility and importance are independent: `critical + cold` is valid.

## Share current state and purpose

Use `get_state(key)`, `list_state(query=...)`, and `set_state(key, value)`. State is mutable current state; durable reasoning belongs in memory.

For an overall purpose, a workflow may choose an ordinary `goal.current` key:

```text
set_state(key="goal.current", value={"purpose":"Deliver the agreed release", "memory_id":"<decision id>"})
get_state(key="goal.current")
```

The server does not automatically read or inject this key into tasks. Agents must retrieve and use it themselves. This release does not add goal enforcement, automatic freshness checks, keyed memory upserts, or context-revision protocols.

To avoid overwriting another agent's update, read the current JSON value, then call:

```text
set_state(key="goal.current", expected_value=<the value just read>, value=<new value>)
```

A mismatch returns `CAS_CONFLICT`; re-read and reconcile instead of retrying a blind overwrite. `list_state(query="goal")` discovers matching keys. Separate `search_state` and `compare_and_set_state` aliases are advanced-only.

## Preserve UNKNOWNs through reobservation

Use `observe_context` to record one explicit observation step. Unexamined possibilities need not become records; save a gap when observation makes it concrete.

```text
observe_context(kind="unknown", topic="Unexplained startup failure", content="Observed failure and the specific unknown.", source_ref="experiment:start-1")
observe_context(kind="hypothesis", parent_id="<unknown id>", topic="Possible cause", content="A provisional explanation and how to test it.")
observe_context(kind="observation", parent_id="<unknown id>", topic="Reproduction result", content="New evidence and observed scope.", source_ref="experiment:start-2", resolves_unknown=true)
list_unknowns()
```

A hypothesis must point to an UNKNOWN. `resolves_unknown=true` is allowed only for an observation whose parent is the UNKNOWN it resolves. If evidence is insufficient, omit that flag. The earlier UNKNOWN body remains preserved; `list_unknowns(include_resolved=true)` includes resolved records. Lineage and source events can be inspected with `get_context_history`.

## Advanced mode

The normal MCP surface has 16 tools listed in the README. Only enable `ANT_MEMORY_ADVANCED_TOOLS=1` for deliberate maintenance or diagnosis. It adds retrieval sessions, deep recall, recall logs, summary/visibility diagnostics, successor lookup, and compatibility aliases. Old instructions are not a reason to route routine work through advanced tools. `delete_context` remains unavailable.
