# ANTmemory 4

Claude Code・Codex・Antigravity・Hermesなどが共有する、履歴を追跡できるローカル永続メモリ。**公開版4.0.0**では本体をRustのdaemonとstdioクライアントへ置き換えました。

[English](README.md) · [ダウンロード](https://github.com/s9t2ksws87-jpg/ANTmemory/releases/latest) · [2.xからの更新](docs/UPGRADING.md)

ANTは **Agent Nest Tool** に由来します。決定・根拠・停止点を持ち寄る記憶の巣です。解決した作業空間ごとにSQLiteのDBを分け、別エージェントが記憶から判断の経緯や現物へ戻れます。

## Windows x64で使う

1. ReleaseのANTmemory-4.0.0-windows-x64.zipとSHA256SUMS.txtを取得します。
2. ハッシュを確認し、ZIP全体を書込可能なフォルダへ展開します。runtime/・models/・stores/・ライセンス類を一緒に置きます。
3. MCP設定へ登録します。パスは展開場所へ変更してください。

```json
{"mcpServers":{"ant-memory":{"command":"C:/tools/ANTmemory/runtime/bin/antmemory.exe","args":["serve"]}}}
```

exe・CPU用ONNX Runtime・固定したINT8 Ruriモデルを同梱します。通常利用にNode.js・npm・Rustコンパイラ・クラウドAPI・初回モデルダウンロードは不要です。

対象プロジェクト内からクライアントを起動します。別フォルダから同じ記憶を使う場合は、そのフォルダの .antmemory-workspace に参照先をUTF-8で1行保存するか、プロジェクト用MCP設定のANT_MEMORY_WORKSPACEへ明示します。変更後は再接続します。不明な接続は _shared を使うと短く知らせます。これは共有の退避先であり、プロジェクト専用DBではありません。

旧node bin/ant-memory.mjsとnode server/native-server.mjsもexeへの中継として使えます。この中継にnpm依存はありません。旧2.xソースはlegacy/v2/へ保存しています。

## 普段の使い方

```text
search_context(query="前に決めたパーサーの方針")
read_context(id="検索結果のread参照をそのまま")
write_context(content="B案に決めた。理由は……。残ったUnknownは……。")
```

保存は **contentだけ** で使えます。題名・接続元・作業空間・記録時Git情報は自動、tags・取手は任意です。

- 検索は既定4候補・全体約1,000推定token。原文の抜粋から、必要な全文や範囲を開けます。
- 訂正・置換・撤回・期限切れでも旧本文と履歴を保持します。物理削除は公開しません。
- 検索から選んで読んだ記憶は、その検索語でも思い出せます。有用性とは別枠で、後からの「無関係」も反映します。
- 作業札は未完了の作業を短く示します。show_goals(id)は閲覧、enter=trueで作業先も選択。全体目標は対応する会話原文の引用を照合できた時だけ正式にし、不明なら候補を残します。
- UNKNOWN・仮説・再観測を分けます。記憶は保存した内容の記録であり、外界の正しさの自動保証ではありません。重要な判断は日付・状態・出典を確認します。

通常19ツールと具体例は [使い方](docs/MEMORY_USAGE.ja.md) を参照してください。一覧には総数と続きが付き、長いRawは範囲を指定できます。自動既読省略は圧縮世代を観測できるC2の同じ接続だけです。C0/C1でも返答を小さくしますが、圧縮後の記憶を仮定しません。

## 閲覧・保守・ビルド

記憶を見る.batでIDE風の読取専用Viewerが127.0.0.1:3737に開きます。閲覧で検索学習やエージェントの作業先は変わりません。

```powershell
./runtime/bin/antmemory.exe viewer
./runtime/bin/antmemory.exe status
./runtime/bin/antmemory.exe doctor --all
```

daemonは点検・WAL書戻し・ログ回転・毎日のバックアップを定期実行し、バックアップを14日分保持します。[構成](docs/ARCHITECTURE.md)

ソースからは現在のstable Rustを使います。配布バイナリはWindows x64向けです。Linux CIの対象はコアとBM25で、Windows DLLの意味検索ではありません。

```powershell
cargo test --workspace --release --locked
cargo clippy --workspace --all-targets --release --locked -- -D warnings
cargo build --release --locked -p am-cli
python scripts/fetch_assets.py
New-Item -ItemType Directory -Force stores, runtime | Out-Null
$env:ANT_MEMORY_ROOT = (Get-Location).Path
./target/release/antmemory.exe install
python scripts/smoke_mcp.py
```

アセット取得は固定ハッシュを検証します。モデル・ランタイムを利用できない場合はBM25動作であると返します。旧2.x移行試験はNode.js 24以上を用意し、python scripts/smoke_mcp.py --migrationで実行します。

**既存2.xは先に [更新手順](docs/UPGRADING.md) を確認してください。** store.dbを自動上書きせず、V4はv4.dbを使います。予行照合・バックアップ・旧書込の停止を確認して切り替えます。ZIPには個人のDB・会話ログ・対応表を含めません。

ANTmemory独自部分はMIT、モデル・ランタイム・依存ライブラリはそれぞれのライセンスです。[第三者表示](THIRD_PARTY_NOTICES.md) · [検証](docs/RELEASE_VALIDATION.md) · [ローカル環境の前提](SECURITY.md)
