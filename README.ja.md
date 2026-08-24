# ANTmemory

複数のAIエージェント（Claude Code / Codex / Antigravity / Hermes）が共有する、監査可能な永続メモリ。

**現在の公開版: ANTmemory v1.0.0。** 最初の公開安定版です。

ANTmemoryの元名称は **Agent Nest Tool** で、その略称が **ANT** です。複数のエージェントが、アリの巣のように区画化された共有領域へ記憶を持ち寄り、それぞれの作業空間に必要な文脈を取り出す構造も、この名前に込めています。

**要件**: Node.js 24 以上（`node:sqlite` を使用。推奨: 24.x LTS）

中身は [agent-memory-store](https://github.com/vbfs/agent-memory-store)（MIT）。サーバー本体は1つだけ入れて全エージェントで使い回し、DBファイルだけを作業空間ごとに物理的に分離する構成です。その上に、独自の監査履歴機能と選択的想起機能を `server/lib/` + `bin/` で実装しています。

このリポジトリは任意の場所に配置できます。`bin/`・Viewer・テストは `ANT_MEMORY_ROOT` 環境変数、またはスクリプト自身の位置からルートを解決します。ワークスペース対応表（実行時生成）は `runtime/workspaces.json`（Git管理外）です。

[English README](README.md)

---

## 何ができるか

### 基本機能（agent-memory-store 由来）

- MCP サーバーとして動作し、`write_context` / `search_context` / `read_context` で記憶の保存・検索・取得を行う
- 検索は BM25（trigram、日本語対応）+ ローカルMiniLM埋め込みによる意味検索のハイブリッド
- 作業空間ごとに DB が完全分離されるため、プロジェクト間で記憶が混ざらない

### 「消さない・監査する」履歴管理

- **物理削除の禁止**: `delete_context` は常時拒否され、代替手段を案内する
- **更新 = `supersede_context`**（理由必須）: 旧記録を残したまま新版へ置き換え、置換チェーンを追跡可能。通常検索では現行版のみ返る
- **撤回 = `retract_context`**（理由必須）: 誤りだった記録を無効化するが本文は残す
- **監査イベント**: 誰が・いつ・なぜ変更したかを `memory_events` に記録
- **記録主体の信頼境界**: actor の正本はサーバー起動時の `ANT_MEMORY_CLIENT`。エージェントが自由入力した `agent` 値は接続元へ正規化され、背後モデル切替（Hermes等）でも actor は不変

### 「忘れないが、必要な時まで思い出さない」選択的想起

- **Progressive Disclosure**: `search_context` の既定返却は metadata + short summary（Level 0-1）。本文は `read_context` / `recall_deep` / `full_content=true` で段階的に取得。KPI実測は **792 → 277 tokens（−65.0%）**、raw改変 0件
- **Visibility Layer**: hot/warm/cold/archive。importance とは独立し、`critical + cold` も許可。通常検索では hot/warm、cold/archive は深掘り時に取得
- **Summary Layer**: 決定論的抽出による要約（LLM不使用）。原文ハッシュ不一致の要約は無効化され、`chunks.content` が常に正本
- **Token Budget / max_results**: 低関連から切り詰め、通知付きで返却量を制御
- **Retrieval Session**: `session_id` により同一タスク中の既読記憶を順位降下
- **Recall理由ログ**: 「なぜ思い出したか」を記録し Viewer で確認可能
- **Visibility推奨**: usage統計に基づく変更推奨。自動適用せず、`get_visibility_recommendations` → 人間判断 → `promote_memory`

リリース検証は [`docs/ARCHITECTURE_VALIDATION.md`](docs/ARCHITECTURE_VALIDATION.md)、他のメモリシステムとの比較調査は [`docs/ARCHITECTURE_RESEARCH.md`](docs/ARCHITECTURE_RESEARCH.md) を参照してください。

---

## クイックスタート

```powershell
git clone https://github.com/s9t2ksws87-jpg/ANTmemory.git
cd ANTmemory\server
npm ci
```

`npm ci` で release lockfile に固定された依存関係を導入し、`postinstall` から `bin/apply-patches.mjs` が実行されます。

エージェントのMCP設定に追加します（Claude Code例）:

```json
{
  "mcpServers": {
    "ant-memory": {
      "command": "node",
      "args": ["<ANTmemoryの絶対パス>\\bin\\ant-memory.mjs"],
      "env": {
        "ANT_MEMORY_CLIENT": "claude-code"
      }
    }
  }
}
```

作業空間判定・DB作成・監査・履歴・visibility・summary・usage・recallスキーマの初期化はサーバー側が自動で行います。エージェント向け運用ルールは [`docs/MEMORY_USAGE.ja.md`](docs/MEMORY_USAGE.ja.md) にまとめています。英語版は [`docs/MEMORY_USAGE.md`](docs/MEMORY_USAGE.md) です。

---

## Viewer

workspace別の記憶、履歴、visibility、参照統計、想起理由を人間が確認するための読み取り専用Viewerを同梱しています。

```powershell
node viewer\server.mjs
```

Windowsでは `viewer\start-viewer.bat` でも起動できます。既定では `127.0.0.1:3737` のみで待ち受け、DBはread-onlyで開きます。詳細は [`viewer/README.md`](viewer/README.md) を参照してください。

---

## 構成

```text
ANTmemory/
├── bin/ant-memory.mjs       起動ラッパ（workspace判定・DB切替）
├── bin/apply-patches.mjs    upstreamへの独自パッチ（冪等・postinstall）
├── bin/migrate-*.mjs        migration（冪等・非破壊）
├── bin/test-*.mjs           隔離ストアを使うテスト群
├── server/                  agent-memory-store本体 + 監査履歴・選択的想起の拡張
│   └── lib/
├── viewer/                  localhost専用read-only Viewer
├── docs/
│   ├── MEMORY_USAGE.md      エージェント運用ガイド（英語）
│   ├── MEMORY_USAGE.ja.md   エージェント運用ガイド（日本語）
│   ├── ARCHITECTURE_VALIDATION.md v1.0.0の設計検証記録
│   └── ARCHITECTURE_RESEARCH.md       比較調査・設計背景
└── stores/                  workspace別DB（Git管理外）
```

このリリースツリーには製品ソース、試験、検証資料、保守対象の運用資料だけを収録し、文書は `docs/` に集約します。

---

## 作業空間の判定ロジック

`bin/ant-memory.mjs` は次の順でworkspaceを判定します。

1. `ANT_MEMORY_WORKSPACE`
   - 絶対パス: フォルダ名をDB名に使用。同名衝突時は短いhashを付け、`runtime/workspaces.json` に固定記録
   - 単なる名前: `stores/<名前>/store.db` として使用。名前指定のみでは `workspaces.json` へ自動登録しない
2. cwdから上へ `.git` / `CLAUDE.md` / `AGENTS.md` / `GEMINI.md` / `.agents` / `.claude` / `package.json` / `pyproject.toml` / `Cargo.toml` / `go.mod` を探索
3. 判定不能または共有領域なら `_shared`

新規workspaceの初回起動時にはDB・監査履歴・選択的想起schema・visibility初期値を自動確保します。`capture-baseline` だけはテスト用ストア混入を避けるため人間が明示実行します。

```powershell
node <ANTmemoryの絶対パス>\bin\ant-memory.mjs --where
```

実際に開いたDBの履歴は `logs/launcher.log` に記録されます。

---

## クライアント

| エージェント | workspace判定 |
|---|---|
| Claude Code / Codex | cwdから自動 |
| Antigravity | IDE内部情報から特定 |
| Hermes | cwd / `ANT_MEMORY_WORKSPACE` |

全クライアントが同じ `bin/ant-memory.mjs` を起動し、`ANT_MEMORY_CLIENT` に自分の名前を渡します。

### Antigravity

Antigravity IDEはworkspace環境変数なしでMCPを自身のinstall directoryから起動するため、ラッパはIDE内部の `workspaceStorage` と language server の `--workspace_id`（folder URIのSHA256）を照合します。単一windowでは確定的に解決し、複数windowでは最終使用windowへfallbackします。この処理はAntigravityの公開APIではなく内部実装に依存するため、将来のIDE更新で変わる可能性があります。

既知の制限: VS Code拡張として動くCodexはworkspaceを特定する手がかりがなく `_shared` に落ちることがあります。workspace内のterminalから起動すれば正しく判定されます。

### Hermes

以下は ANTmemory の接続検証で確認した Hermes 固有の注意点です。Hermes 側の実装変更で挙動が変わる可能性があります。

1. **`hermes mcp add --env` を環境変数設定として信用しない。** 検証した Hermes では `--env KEY=VALUE` が `env` ではなく、MCPサーバーの `args` に文字列 `--env`, `KEY=VALUE` として保存されました。その結果 `ANT_MEMORY_CLIENT` が渡らず actor が `unknown` になります。`hermes mcp add` 後に Hermes の `config.yaml` を確認し、必要なら手動修正してください。Windows の標準的な保存先例は `$HOME\AppData\Local\hermes\config.yaml` です。
2. **`config.yaml` の `env` は辞書形式にする。** `- ANT_MEMORY_CLIENT=hermes` のようなリスト形式では接続に失敗しました。

```yaml
ant-memory:
  command: node
  args:
    - <ABSOLUTE_PATH_TO_ANTMEMORY>/bin/ant-memory.mjs
  env:
    ANT_MEMORY_CLIENT: hermes
```

3. **actor 警告は設定ミスの診断に使える。** `write_context` などで `agent="hermes"` と申告していても、接続元が `unknown` なら ANTmemory は申告値を信用せず接続元へ正規化します。この警告が出たらまず `ANT_MEMORY_CLIENT` の設定を確認してください。
4. **Hermes の patch ツールから `config.yaml` を書けない場合がある。** 検証環境ではセキュリティガードにより拒否されたため、設定修正はターミナルや通常のテキストエディタから行いました。
5. **設定変更後は MCP を再読み込みする。** 既存セッションには反映されません。`hermes mcp test <name>` で接続を確認してから新規セッションを開きます。
6. **ツール名 prefix は `mcp__ant_memory__*`。** MCP名 `ant-memory` のハイフンはツール名ではアンダースコアへ正規化されます。
7. **cwd 起点の workspace 判定には起動場所が重要。** Hermes をホームディレクトリ等から起動し、上位探索で `.git` 等の目印が見つからない場合は `_shared` へフォールバックします。対象プロジェクトをカレントディレクトリにするか、`ANT_MEMORY_WORKSPACE` を明示してください。

---

## upstreamに当てている独自パッチ

`agent-memory-store` 0.0.9 に対して `bin/apply-patches.mjs` が次を冪等に修正します。

1. ハイフン入りFTS5検索語の構文エラー
2. 日本語検索用 `trigram` tokenizer
3. 既存DBのFTS索引再構築
4. TTL比較の時刻書式ずれ
5. 旧形式migrationの実行順序
6. 未実装だった `min_score`
7. RRF差を潰していた早すぎるscore丸め
8. migration時のDate型binding失敗
9. concurrent write時の `SQLITE_BUSY`（`busy_timeout=5000`）

対象upstream versionが変わってパッチ元コードが一致しない場合は、黙って続行せずエラー終了します。

---

## テスト

```powershell
node bin\test-baseline.mjs
node bin\test-actors.mjs
node bin\test-lifecycle-search.mjs
node bin\test-viewer.mjs
node bin\test-visibility.mjs
node bin\test-summaries.mjs
node bin\test-token-budget.mjs
node bin\test-progressive-disclosure.mjs
node bin\test-usage-sessions.mjs
node bin\test-visibility-recommendations.mjs
node bin\test-field.mjs
```

設計検証報告: [`docs/ARCHITECTURE_VALIDATION.md`](docs/ARCHITECTURE_VALIDATION.md)。v1.0.0の検証では12 suite / 201 caseが全passしています。

---

## ライセンス

ANTmemory独自部分はMIT。第三者ソフトウェアについては [`THIRD_PARTY_NOTICES.md`](THIRD_PARTY_NOTICES.md) を参照してください。

## メンテナンス

- server更新: upstream互換性を確認してから更新する。`apply-patches.mjs` は未対応versionで失敗する
- backup: MCPを停止して `stores/` をコピーするのが最も単純。migration scriptは実行前backupも作成する
- security/privacy: `stores/`, `runtime/`, `logs/`, `backups/` は公開・commitしない。詳細は [`SECURITY.md`](SECURITY.md)
