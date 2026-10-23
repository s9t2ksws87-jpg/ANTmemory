# ANTmemory

Claude Code・Codex・Antigravity・Hermesなど、複数のMCPクライアントが共有する、履歴を追跡できる永続メモリ。

**現在の公開版: 2.0.0。** [English](README.md) · [1.0.0からの更新](docs/UPGRADING.md)

ANTの元名称は **Agent Nest Tool** です。エージェントが記憶を持ち寄るアリの巣をイメージし、作業空間ごとにSQLiteのDBファイルを物理分離しています。あるエージェントが残した決定を別のエージェントが読み、その根拠や変更履歴も確認できます。

2.0.0ではMCPサーバー・保存・移行・検索の実装をANTmemory側で所有します。`agent-memory-store` の実行時導入やインストール時パッチは不要になりました。由来とライセンス表記は [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) に残しています。

## できること

- 同じ作業空間を選択したクライアント間で、記憶と現在状態を共有します。記録主体は接続元の `ANT_MEMORY_CLIENT` で決まります。
- 訂正・置換・撤回・期限切れでも旧本文を保持し、理由と監査イベントを残します。物理削除ツールは公開しません。
- 通常検索ではactiveかつhot/warmの記録をmetadataと短い要約で返します。全文・履歴・cold/archiveは必要時に指定できます。
- 日本語に対応するtrigram FTS5/BM25とローカルMiniLM埋め込みを組み合わせます。モデルを利用できない場合はBM25へフォールバックします。
- 観測・明示したUNKNOWN・仮説・再観測を別記録として保存し、`source_ref` でRawへ戻る取手を残せます。
- 通常のエージェント向けは **16ツール**。旧互換・保守・診断用ツールは明示的なadvanced設定で追加します。

記憶は「何を記録したか」の証拠です。外界の真実を自動保証するものではありません。検索スコアも候補内の相対値で、`1.0` は正しさや関連性の保証ではありません。重要な判断では日時・現行状態・元の根拠を再確認します。

## 導入

**Node.js 24以上**とnpmが必要です。

```sh
git clone https://github.com/s9t2ksws87-jpg/ANTmemory.git
cd ANTmemory/server
npm ci
```

クライアントのMCP設定に登録します。パスと接続元名は環境に合わせて変更してください。

```json
{
  "mcpServers": {
    "ant-memory": {
      "command": "node",
      "args": ["<ABSOLUTE_PATH_TO_ANTMEMORY>/bin/ant-memory.mjs"],
      "env": {
        "ANT_MEMORY_CLIENT": "claude-code"
      }
    }
  }
}
```

対象プロジェクト内からクライアントを起動するか、`env` に `ANT_MEMORY_WORKSPACE` と作業空間の絶対パスを指定します。同じDBを共有するには、各クライアントが同じ作業空間を選択する必要があります。設定変更・更新後はMCP接続を再起動します。

初回は `Xenova/all-MiniLM-L6-v2` をダウンロードします。fp32 ONNXファイルは約90 MBです。完了したダウンロードだけをキャッシュに採用し、中断したファイルは再利用しません。キャッシュはtransformersランタイムのcacheディレクトリ内の `antmemory-atomic` に保存します。モデルの読込状況はstderrへ出力します。

## 通常16ツール

| 用途 | ツール |
|---|---|
| 検索・保存・取得 | `search_context`, `write_context`, `read_context`, `list_context` |
| 履歴を残した更新 | `correct_context`, `supersede_context`, `retract_context`, `restore_context`, `change_importance`, `get_context_history` |
| 可視性 | `set_visibility` |
| 現在の共有状態 | `get_state`, `set_state`, `list_state` |
| 観測の循環 | `observe_context`, `list_unknowns` |

検索件数は `top_k`、概算token上限は `token_budget` です。必要に応じて `full_content`・`include_history`・`include_hidden` を使います。`read_context` はRaw全文とstatus・visibility・current・日時・存在時のsource_refを一緒に返します。

軽微訂正は `correct_context` で、省略したmetadataを継承して新版を作ります。内容全体の置換は `supersede_context` で、新記録のmetadataを明示します。どちらも旧本文を残します。訂正・置換・撤回・復元・重要度・可視性の変更には理由が必要です。`delete_context` はadvancedでも公開しません。

stateは現在状態の共有黒板です。例えば `goal.current` に全体目的へのポインタを置けますが、これは通常のstateキーであり、自動作成・自動注入・強制適用はしません。同時更新には `set_state(expected_value=...)` を使い、経緯や決定理由は記憶側へ残します。

詳しい例は [メモリの使い方](docs/MEMORY_USAGE.ja.md) を参照してください。

## 作業空間の選択

選択順は次の通りです。

1. `ANT_MEMORY_WORKSPACE` の明示指定（絶対パスまたはstore名）。
2. cwdから上方向へ探索して見つかったプロジェクト目印。
3. cwdから特定できない場合、検証を通ったworkspace環境変数。
4. 対応するWindows環境でのAntigravity内部workspace情報。
5. 特定できなければ `_shared`。

絶対パスとstore名の対応は `runtime/workspaces.json` に記録し、同名衝突時は短いハッシュを付けます。単なるstore名は `stores/<name>/store.db` を直接選択します。`_shared` は実際に共有される退避先です。利用前に選択先を確認できます。

```sh
node /absolute/path/to/ANTmemory/bin/ant-memory.mjs --where
```

Antigravityの検出はIDE内部情報に依存し、複数ウィンドウで曖昧な場合は最近使った作業空間を選ぶことがあります。確実に指定する場合は `ANT_MEMORY_WORKSPACE` を使います。IDE本体やホームから起動したクライアントも `_shared` へ落ちる場合があります。実際の選択は `--where` と `logs/launcher.log` で確認します。

Hermesでは保存後の設定を確認し、`env` が `ANT_MEMORY_CLIENT` を含むmappingになっていることを確認してください。過去の観測では `--env` の値が `args` に保存されるビルドがありました。

## Viewer

```sh
node viewer/server.mjs
```

`127.0.0.1:3737` で起動する読取専用Viewerです。作業空間・記憶・履歴・可視性・usage・想起理由を確認できます。Windowsでは `viewer/start-viewer.bat` も使えます。[詳細](viewer/README.md)

## 保守と検証

```sh
npm run test:all --prefix server
npm audit --omit=dev --audit-level=high --prefix server
```

試験は分離storeで実行し、詳細をGit管理外の `test-logs/` に保存します。Raw保持、検索、native MCP、通常16ツール、観測の系譜、全体目的と出典、Viewer、同時書込、モデルキャッシュを検証します。実際の仕事への効力とは区別して [検証記録](docs/RELEASE_VALIDATION.md) に範囲を残します。

保守で必要な場合のみMCPサーバーに `ANT_MEMORY_ADVANCED_TOOLS=1` を指定します。旧互換alias、深掘り検索/session、要約診断、想起ログ、可視性推奨などが追加されます。通常作業は16ツールで行います。

`stores/`・`runtime/`・`logs/`・`backups/`・依存関係・生成した試験データはGit管理外です。更新前は書込プロセスを止め、storeをバックアップします。[更新手順](docs/UPGRADING.md) · [変更履歴](CHANGELOG.md) · [SECURITY.md](SECURITY.md)

旧1.0.0の検証は [ARCHITECTURE_VALIDATION.md](docs/ARCHITECTURE_VALIDATION.md) に歴史資料として保持しています。2.0.0の計測ではありません。[設計比較](docs/ARCHITECTURE_RESEARCH.md)

## ライセンス

ANTmemory所有部分はMITです。第三者部分はそれぞれのライセンスに従います。[LICENSE](LICENSE) · [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md)
