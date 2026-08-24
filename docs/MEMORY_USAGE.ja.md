# ant-memory の使い方（エージェント共通ルール）

複数のAIエージェント（Claude Code / Codex / Antigravity / Hermes）が、同じ作業空間の記憶を共有するための永続メモリ。作業空間ごとにデータベースが物理的に分かれているので、別プロジェクトの記憶が混ざることはありません。

保存先: `<ANTMEMORY_ROOT>\stores\<ワークスペース名>\store.db`

English version: [`MEMORY_USAGE.md`](MEMORY_USAGE.md)

---

## 1. 使えるツール

| ツール | 用途 |
|---|---|
| `search_context` | 作業前に引く。既定では現在有効な記憶の metadata + short summary のみ返す。`full_content=true` で本文ごと返す |
| `write_context` | 決定事項・発見・ユーザーの好みを残す |
| `read_context` | `id` で全文を取り出す |
| `list_context` | 何が入っているかの一覧（本文なし・棚卸し用） |
| `supersede_context` | 記録の置き換え。旧記録は superseded になり本文は残る（reason 必須） |
| `retract_context` | 記録を撤回済みにする。本文は消えない（reason 必須） |
| `change_importance` | 重要度の変更（reason 必須） |
| `restore_context` | superseded/retracted を active へ戻す（reason 必須） |
| `get_context_history` | 記録の変更履歴（作成・訂正・置換）を時系列表示 |
| `get_successor_context` | 置換チェーンを辿って現行版を確認 |
| `promote_memory` | visibility（hot/warm/cold/archive）の変更。importanceと独立（reason 必須） |
| `get_memory_summary` | short/detailed 要約の取得。原文は read_context で取得 |
| `recall_deep` | cold/archive も含む段階的深掘り。Stage1=候補一覧、Stage2=resolve+idsで本文 |
| `begin_retrieval_session` | 検索セッション開始。session_id を検索に渡すと既読の順位が下がる |
| `get_recall_log` | 「なぜ思い出したか」の想起履歴 |
| `get_visibility_recommendations` | usage統計に基づくvisibility変更の推奨（自動適用はされない） |
| `get_state` / `set_state` | 作業の途中状態、進捗フラグ、カウンタ |

`delete_context`（物理削除）は使いません。呼び出しても拒否され、`supersede_context` / `retract_context` が案内されます。更新は `supersede_context`、撤回は `retract_context` を使い、どちらも理由を記録します。

### 選択的想起の検索パラメータ

```text
search_context(
  query: "...",
  include_history: true,   // superseded/retracted/expired も含める
  include_hidden: true,    // cold/archive も含める
  full_content: true,      // 本文全文を返す（既定は summary のみ）
  max_results: 5,          // 返却件数の上限
  token_budget: 500,       // 返却量の上限（概算token）
  session_id: "...",       // セッション内既読の順位を下げる
)
```

---

## 2. いつ検索するか

タスクを始める前に、過去の判断が関係しそうなら `search_context` を引きます。特に次の場合です。

- ユーザーが「前に言ったやつ」「いつもの」「この前の続き」と言ったとき
- 設定・仕様・命名・文体など、過去に決めた可能性があることに触れるとき
- 同じファイルや同じ題材を再び触るとき
- 前回どこまで進んだか分からないとき（`get_state` も併用）

検索は1回で諦めず、必要なら語を変えて2〜3回試します。`search_mode` は既定の `hybrid`（キーワード＋意味検索）で構いません。

`min_score` は「最良ヒットを1.0としたときの相対的な近さ」で足切りします。広く見たいときは `min_score` を下げるか `top_k` を増やします。

### 検索のコツ

- キーワード検索は3文字以上の語が安定します。索引が3文字単位（trigram）のためです
- 文章のまま検索して構いません。`hybrid` では意味検索も働きます
- ハイフンを含む語（`ant-memory` など）はそのまま検索できます

---

## 3. いつ書くか

「次に来たエージェントが知らないと同じ質問・調査を繰り返す」ことは記録します。

書くべきもの:

- ユーザーが下した決定と、その理由
- ユーザーの好み・NG・継続的な制約
- 試して失敗したこと
- リポジトリにまだ明示されていない持続的なプロジェクト知識
- 環境・ツール固有の癖や手順
- 長い作業の区切りごとの現在地（`set_state` と併用）

書かないもの:

- リポジトリやファイルを読めば分かること
- その場限りの些末なやりとり
- パスワード・APIキー・トークンなどの秘密情報
- 未確定の憶測を確定事実のように書いたもの

---

## 4. 書き方の規約

```text
write_context(
  topic:      "何についての記録か（短く具体的に）",
  content:    "本文。後で読んで意味が通るように、背景と結論を含める",
  agent:      "claude-code" | "codex" | "antigravity" | "hermes",
  tags:       ["分類", "作品名", "対象"],
  importance: "low" | "medium" | "high" | "critical",
  ttl_days:   30
)
```

- `topic`: 「第三話の視点」のように具体的にする。「メモ」「作業」だけの名前は避ける
- `content`: 単語の羅列ではなく、決定 + 理由をセットで書く。後から読んでも意味が復元できる文にする
- `agent`: 正本は接続元（`ANT_MEMORY_CLIENT`）。異なる名前を申告すると接続元へ正規化されるため、通常は省略してよい
- `tags`: 後で絞り込む鍵。作品名・話数・分野などを入れる
- `importance`:
  - `critical` … 破ると作品や作業が壊れる大原則
  - `high` … 明示的に決めた持続的な方針
  - `medium` … 通常の作業記録（既定）
  - `low` … 参考程度のメモ
- `ttl_days`: 一時的な情報に付ける。恒久的な決定には付けない

### 内容が変わったとき

物理削除はせず、履歴として訂正します。

- 方針変更など記録の置き換え → `supersede_context`
- 記録自体が誤りだった → `retract_context`
- 重要度だけ変わった → `change_importance`
- superseded/retracted から戻す → `restore_context`
- 変更履歴を見る → `get_context_history`

```text
old = search_context("文体の方針")
supersede_context(old_id, 新topic, 新content, reason: "ユーザーがB案に変更")
get_successor_context(old_id)
search_context("文体の方針", include_history=true)
```

検索は既定で現在有効な記憶のみを返します。過去版・撤回済み・期限切れを見るときは `include_history: true` を付けます。

---

## 5. 作業状態の受け渡し

長い作業を中断・再開するとき、またはエージェント間で引き継ぐとき:

```text
set_state("現在地", { 話数: 3, 段階: "初稿完了", 次: "推敲" })
get_state("現在地")
```

`state` は上書き式の変数です。履歴を残したいことは `write_context` を使います。

---

## 6. 運用メモ（人間向け）

- サーバー本体: `<ANTMEMORY_ROOT>\server`
- 起動ラッパ: `<ANTMEMORY_ROOT>\bin\ant-memory.mjs`
- どのワークスペースと判定されたかの確認:

```powershell
cd <確認したいフォルダ>
node <ANTMEMORY_ROOT>\bin\ant-memory.mjs --where
```

- 起動履歴のログ: `<ANTMEMORY_ROOT>\logs\launcher.log`
- ワークスペース名とパスの対応表: `<ANTMEMORY_ROOT>\runtime\workspaces.json`（実行時に自動更新）
- 判定を手動で固定したいときは環境変数 `ANT_MEMORY_WORKSPACE` を指定する

データベース、runtime mapping、ログ、backup、credentialはGitへcommitしないでください。詳細は [`../SECURITY.md`](../SECURITY.md) を参照してください。
