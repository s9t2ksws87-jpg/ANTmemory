> Historical research snapshot dated 2026-08-22. External project descriptions and proposed architecture below are not a current release specification. See [../README.md](../README.md) for shipped behavior.

# 選択的想起アーキテクチャ設計調査: 4プロジェクト比較

作成日: 2026-08-22
対象: MemOS / OMEGA / Mem0 / Basic Memory

目的: 機能をそのまま輸入するのではなく、どこを採用し、どこを捨てるかを説明する。ANTmemoryの不変条件（append-only history / raw preservation / actor+reason provenance / importance≠visibility / workspace共有DB / 4クライアント対応）は維持する。

---

## 1. 各プロジェクトの調査結果

### 1.1 MemOS (MemTensor/MemOS)

- **保存方式**: MemCube という標準化抽象でテキスト/活性化/パラメータ記憶を統合管理。階層的メモリ（作業・長期・外部）をOS的にスケジューリング。
- **retrieval**: メモリ階層間のスケジューリング＋検索。full-context投入より高精度・低レイテンシと報告。
- **token効率**: 全文履歴投入の代替として intelligent memory organisation を提示。
- **history**: MemCube単位のライフサイクル管理。chunk粒度の監査履歴は薄い。
- **multi-agent**: cube単位の所有者管理。actor provenanceの概念は弱め。

### 1.2 OMEGA (omega-memory/omega-memory)

- **保存方式**: SQLite (memories | edges | embeddings)。local-first、MCPサーバー経由。
- **retrieval**: semantic search + embeddings + graph edges。25 core tools。
- **token効率**: `omega_welcome`（セッション開始時の軽量briefing）、checkpoint/resume。
- **history**: TTL / decay / compaction による「intelligent forgetting」を掲げる。クラスタリング要約 (`omega compact`) で関連記憶をまとめて圧縮する。
- **multi-agent**: MCPクライアント全般に対応。主体の厳密な識別は任意。

### 1.3 Mem0 (mem0ai/mem0)

- **保存方式**: ADD-only extraction（2026年4月〜）。1回のLLM呼び出しで抽出、UPDATE/DELETEなし、蓄積型。
- **retrieval**: semantic + BM25 + entity matching（graph memory layer）+ temporal reasoning。
- **token効率**: retrieval budget の概念が明確。LoCoMo 92.5 / LongMemEval 94.4 を full-context比3-4x低tokenで達成と公表。ベンチマーク群（memory-benchmarks）を公開。
- **history**: temporal metadataをextraction時に書き込みretrieval時にスコア化。監査(actor/reason)ではなく時間推論のためのもの。

### 1.4 Basic Memory (basicmachines-co/basic-memory)

- **保存方式**: 人間可読なMarkdownファイルが正本。Obsidian等と同一ファイルを直接読み書き。
- **retrieval**: Markdown内の Relations（`[[リンク]]`）による知識グラフ + 検索。
- **token効率**: 明示的なbudget制御よりも、「人間が必要なノートだけ開く」モデル。
- **history**: ファイルシステム依存。監査履歴機能なし。
- **強み**: relationの人間可読性。`pairs_well_with [[X]]` 形式は非技術者にも読める。

---

## 2. 評価軸マトリクス

| 評価軸 | MemOS | OMEGA | Mem0 | Basic Memory | ANTmemory |
|---|---|---|---|---|---|
| raw不変 | △(要約統合あり) | ✗(compaction) | ○(ADD-only) | ○(Markdown正本) | ◎ |
| 履歴保持 | △ | ✗(TTL削除) | ○(temporalのみ) | ✗ | ◎ |
| retrieval多段 | ○ | ○ | ◎ | △ | ◎（実装済み） |
| token効率 | ◎ | ○ | ◎ | ✗ | ○（KPI実測） |
| visibility概念 | △(階層だが別物) | △(decay) | ✗ | ✗ | ◎（importanceと独立） |
| actor/reason provenance | ✗ | ✗ | ✗ | ✗ | ◎ |
| local-first | ◎ | ◎ | △(cloud併存) | ◎ | ◎ |
| viewer | ○ | △ | ○(dashboard) | ◎(Obsidian) | ○ |

ANTmemoryはまず provenance・raw不変・監査性を確立し、その不変条件を維持したまま token効率・visibility・retrieval多段を追加した。

---

## 3. 採用した点

1. **Progressive Disclosure の Level 構造**（MemOSの階層提示 + OMEGAのwelcome briefing）
   - 検索結果をインデックス（metadata+summary）に留め、必要時に本文へ進む。
2. **token budget の明示的制御**（Mem0）
   - budget超過時の切り詰め順を定義し、切り詰めを呼び出し側へ明示する。
3. **multi-signal ranking**（Mem0: semantic+BM25+temporal+entity / OMEGA: graph edges）
   - ANTmemoryでは semantic+BM25+visibility+usage(+session novelty) を組み合わせる。
   - entity linking はコスト対効果が未実証のため現行公開版には入れなかった。
4. **usage tracking / session既読管理**（OMEGAのcheckpoint・Mem0のaccess系メタデータ）
   - 同一タスク中の重複投入を抑える補助信号として採用。
5. **ADD-only思想の再確認**（Mem0）
   - 「消さない」方向はANTmemoryと一致。ただしANTmemoryでは actor/reason/supersedes/retract/raw preservation を監査可能な履歴として保持する。
6. **人間可読なrelation表示**（Basic Memory）
   - Viewerの supersedes / recall-reason 表示を人間が読みやすい形にする方針へ反映。

## 4. 採用しなかった点

| 機能 | 出典 | 不採用理由 |
|---|---|---|
| compaction / クラスタ要約での記憶置換 | OMEGA | summaryを正本化しかねず、raw preservationと衝突する |
| TTL / decay による物理削除 | OMEGA | 物理削除禁止と衝突するため、ANTmemoryではexpired状態遷移を使う |
| ADD-only extraction（LLM自動抽出） | Mem0 | 書込時LLMによるコスト・レイテンシ・誤抽出を避け、AI自身が明示的にwrite_contextする設計を維持 |
| entity linking / knowledge graph | Mem0, OMEGA | 効果が未実証のまま実装コストが大きい。将来候補 |
| MemCube的なメモリ階層の全面導入 | MemOS | 既存のworkspace共有DB構造と競合。visibility layerで必要な部分だけ吸収 |
| Markdown正本への変更 | Basic Memory | SQLite+raw+audit historyを維持 |
| 自動人格形成・ユーザー像推定 | 業界一般 | 誤学習リスクが高く、現行公開版の対象外 |
| coordination / task queue 統合 | OMEGA | オーケストレーションはメモリ本体とは別レイヤーとした |

## 5. ANTmemory固有の差分

- 参照先はいずれも「なぜそれを思い出したか」を人間に説明することを中心機能にしていない。ANTmemoryは `memory_recall_log` を追加し、保存・変更の監査思想をretrieval側まで拡張した。
- token削減は印象ではなく固定コーパスで計測し、通常検索の平均返却量を 792 → 277 estimated tokens（−65.0%）まで削減した。
- visibility を importance と独立させることで、「重要だが毎回は要らない」記憶を `critical + cold` のように表現できる。
- visibility recommendation は自動適用せず、人間または呼び出し側workflowが最終判断する。

---

## 6. 実装結果への引き継ぎ

公開安定版 v1.0.0 では visibility / summaries / usage / retrieval sessions / recall log を統合し、raw memoryを正本として維持した。summary生成はLLMを使わない決定論的抽出とし、usageは補助信号に留めることで自己強化ループを避けている。

実測結果は [`ARCHITECTURE_VALIDATION.md`](ARCHITECTURE_VALIDATION.md) を参照。
