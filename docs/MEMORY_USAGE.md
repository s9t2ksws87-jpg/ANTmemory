# Operating ANTmemory 4

Search at task start or when unfamiliar changes suggest prior work. Repeat with different terms only if needed. Save decisions with reasons, lessons and stopping points that another agent needs, rather than copies of entire files.

## Save and recall

Only write_context(content) is required. Its first nonempty line supplies the title; client, workspace and Git provenance are automatic. Tags, anchors, source_ref, importance and TTL are optional.

search_context(query) defaults to four excerpts within ~1,000 estimated tokens. top_k, token_budget, full_content, include_history, include_hidden and verbose are optional controls. Scores are relative, not confidence.

Pass the read reference unchanged into read_context(id). It carries memory ID + search ID across reconnections. A later read with the same reference and verdict="not_relevant" reduces recall words too; a withdrawn association is not a deletion or a global ban on literal body matches.

```text
read_context(id="memory-id@search-id")
read_context(id="memory-id", around=2487)
read_context(id="memory-id", start=2487, length=400)
read_context(id="memory-id", token_budget=600)
```

Ranges are UTF-16 positions into original Raw, not summaries. Omitted ranges read the full body. verbose=true adds recorded Git, anchors and learning detail. Source handles are re-observed now; an agent's report is distinguishable from independently observed execution.

## Preserve history and UNKNOWNs

Correct with correct_context, replace with supersede_context, withdraw with retract_context, restore an eligible withdrawn/expired record without a successor with restore_context. Reasons are required. change_importance, set_visibility and get_context_history operate independently. Do not delete and recreate a record to hide a mistake.

write_context(kind="unknown"/"hypothesis"/"observation", ...) uses the same implementation as observe_context. Resolve an UNKNOWN with a new observation and parent_id/resolves_unknown=true. Do not overwrite the original body. list_unknowns defaults to current unresolved records.

## Work and state

post_goal(title) posts unfinished work; finish with close_goal(id), using done/dropped/merged. There is no pause ritual. parent declares child work. show_goals(id) views the subtree; enter=true or search_context(goal=id) selects work. Other work does not silently become yours. trace=true requires a target card. Overall goals need verified user conversation evidence; unmatched quotes remain proposals.

State is a current blackboard: get_state, set_state, list_state. expected_value provides CAS. Keep durable reasoning in memory. goal.current remains compatible ordinary shared state, not an authorized overall goal.

## Bounds and routing

list_context defaults to 20 rows/~1,200 tokens, list_unknowns to ten/~1,000, list_state to 20/~1,000. Follow the returned offset with the same filters. Goal pages carry revision; restart after changes. Unshown data is not marked seen.

Budgets use a CJK/UTF-16 estimate, not provider billing or a model tokenizer. Full Raw can be long. C2 can omit sent bodies only while a conversation generation is observable; C0/C1 never assume memory survived compaction. fresh=true resends. Manual known_revisions is optional and must be cleared after context loss.

Routing priority: explicit ANT_MEMORY_WORKSPACE; local .antmemory-workspace; project markers above cwd; validated hints; supported Antigravity metadata; _shared. The local link is one UTF-8 path, relative to that file when relative. Invalid links return a reason; unresolved routing is announced once per connection and is not project isolation.
