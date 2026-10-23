# ANTmemory 2.0の使い方

[English](MEMORY_USAGE.md) · [導入と作業空間の選択](../README.ja.md)

## 作業前に検索する

タスク開始時に `search_context` で関連する決定・制約・教訓を検索します。見つからなければ語を変えて2〜3回検索します。検索結果は過去の記録であり、その内容が現在も正しいと確定したわけではありません。

```text
search_context(query="公開版 更新方針", top_k=6, token_budget=1200)
read_context(id="<検索で得たID>")
```

通常検索はactiveかつhot/warmのmetadataと短い要約です。必要時に `include_history=true`、`include_hidden=true`、`full_content=true` を指定します。件数指定は `top_k` のみで、`max_results` は旧仕様です。token上限は概算で、切詰め時には通知があります。`relative_score` は候補内の相対値です。

`read_context` ではRaw全文、status、visibility、current、作成/更新日時、存在時のsource_refを確認できます。棚卸しは `list_context` です。重要な判断では元の根拠を再観測します。

## 次のエージェントが必要とすることを残す

ユーザーの決定と理由、継続的な好み・NG、失敗と教訓、環境の癖、長期作業の重要な現在地を保存します。秘密情報、その場限りの会話、管理されたファイルを読めば分かることは残しません。

```text
write_context(
  topic="公開時に保持する境界",
  content="決定と理由、観測した範囲、残っている不明点。",
  tags=["公開", "決定"],
  importance="high",
  source_ref="commit:<commit-id>"
)
```

`source_ref` は後からRawへ戻るための取手で、参照先を自動取得・検証する機能ではありません。ファイル・コミット・実験・会話・URLなどを指定します。一時情報だけに `ttl_days` を付けます。記録主体は `ANT_MEMORY_CLIENT` で決まるため、書込に `agent` / `actor` を入力しません。検索時の `agent` filterは引き続き使えます。

## 消さずに更新する

| 状況 | 操作 |
|---|---|
| 軽微訂正 | `correct_context(id, 変更する項目..., reason)` |
| 方針・内容全体の置換 | `supersede_context(old_id, topic, content, metadata..., reason)` |
| 誤記録の撤回 | `retract_context(id, reason)` |
| 重要度変更 | `change_importance(id, importance, reason)` |
| hot/warm/cold/archiveの変更 | `set_visibility(id, visibility, reason)` |
| successorのないretracted/expiredを戻す | `restore_context(id, reason)` |
| 変更履歴と関係の確認 | `get_context_history(id)` |

訂正では新しい版を作り、省略した題名・本文・tags・importance・source_ref・元の絶対期限を継承します。期限を解除する場合は `clear_ttl=true` を使います。`ttl_days` とは同時指定できません。全体置換は新記録のmetadataを意識して指定します。

successorのあるsuperseded記録は現行へ戻せません。削除して再作成する運用は禁止です。物理削除ツールは公開しません。visibilityとimportanceは独立しており、`critical + cold` も有効です。

## 現在状態と全体目的を共有する

stateは `get_state(key)`、`list_state(query=...)`、`set_state(key, value)` の3ツールで扱います。stateは現在状態の共有黒板で、経緯や決定理由は記憶へ残します。

全体目的には通常のキー `goal.current` を選べます。

```text
set_state(key="goal.current", value={"purpose":"合意した公開版を届ける", "memory_id":"<決定のID>"})
get_state(key="goal.current")
```

サーバーが自動的にこのキーを取得・注入するわけではありません。エージェントが読み、作業に使う必要があります。この公開版には目的の強制適用、自動鮮度判定、key付き記憶upsert、context revisionプロトコルは含みません。

同時更新の取りこぼしを防ぐ場合は現在のJSONを読み、その値を渡します。

```text
set_state(key="goal.current", expected_value=<直前に読んだ値>, value=<新しい値>)
```

一致しなければ `CAS_CONFLICT` です。再読して調整し、無条件で上書きしません。キー探索は `list_state(query="goal")` です。別名の `search_state` / `compare_and_set_state` はadvanced専用です。

## UNKNOWNを残して再観測する

`observe_context` で観測の一段階を保存します。観測で具体的になった不明点をUNKNOWNとして分離します。まだ何も観測していない可能性まで記録する必要はありません。

```text
observe_context(kind="unknown", topic="起動失敗の原因", content="何を観測し、何が分からないか。", source_ref="experiment:start-1")
observe_context(kind="hypothesis", parent_id="<UNKNOWNのID>", topic="原因の仮説", content="仮の説明と検証方法。")
observe_context(kind="observation", parent_id="<UNKNOWNのID>", topic="再現試験の結果", content="得られた根拠と観測範囲。", source_ref="experiment:start-2", resolves_unknown=true)
list_unknowns()
```

hypothesisの親はUNKNOWNである必要があります。`resolves_unknown=true` は、親UNKNOWNを解決したobservationにだけ付けます。根拠が足りなければ付けません。旧UNKNOWN本文は保持されます。解決済みも見る場合は `list_unknowns(include_resolved=true)`、系譜と出典イベントは `get_context_history` です。

## advanced

通常はREADMEにある16ツールで作業します。保守・診断時だけ `ANT_MEMORY_ADVANCED_TOOLS=1` を明示すると、retrieval session、深掘り想起、想起ログ、要約・可視性診断、successor参照、旧互換aliasが追加されます。古い手順を理由に通常作業をadvancedへ迂回しません。`delete_context` はadvancedでも公開しません。
