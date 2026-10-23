> Historical evidence for ANTmemory 1.0.0. For current release checks, see [RELEASE_VALIDATION.md](RELEASE_VALIDATION.md).

# ANTmemory v1.0.0 アーキテクチャ検証報告

作成日: 2026-08-22
対象: ANTmemory v1.0.0 公開検証

---

## 1. 試験環境

- 隔離ストア `stores/field-test/`（本番データには触れない）
- 5 MCPクライアントプロセス: claude-code / codex / antigravity / hermes×2（背後モデル切替）

## 2. シナリオ別結果（`bin/test-field.mjs` — 13/13 PASS）

| シナリオ | 内容 | 結果 |
|---|---|---|
| S1 通常タスク | 4クライアント同時書込25件 → 選択的想起検索（Level0-1） | ✅ 書込エラー0・検索正常 |
| S2 長時間タスク | retrieval session で既読記憶の順位を下げる | ✅ ペナルティ動作 |
| S3 継続作業 | supersede チェーン後、通常検索で新版のみ | ✅ 旧版非表示・旧本文は保持 |
| S4 履歴深掘り | cold に沈めた記憶を recall_deep で取得 | ✅ 候補含む・resolve本文取得 |
| S5 Hermesモデル切替 | model alpha→beta をまたぐ書込 | ✅ actor=hermes で連続 |

完了条件の実測:

- 記憶欠損: **0件**（報告ID全件 read back 成功）
- raw改変: **0件**（DB一貫性ハッシュ照合）
- integrity_check: **ok** / 外部キー違反: **0**
- retrieval failure: 許容範囲内（全シナリオで必要記憶へ到達）

## 3. 全試験スイート結果

| 試験 | ケース | 結果 |
|---|---|---|
| test-baseline.mjs（基準回帰） | 14 | ✅ 全パス |
| test-lifecycle-search.mjs（検索回帰） | 10 | ✅ 全パス |
| test-actors.mjs（主体信頼性） | 9 | ✅ 全パス |
| test-viewer.mjs（ビューア） | 9 | ✅ 全パス |
| test-visibility.mjs | 10 | ✅ 全パス |
| test-token-budget.mjs | 7 | ✅ 全パス |
| test-summaries.mjs | 11 | ✅ 全パス |
| test-progressive-disclosure.mjs | 11 | ✅ 全パス |
| test-usage-sessions.mjs | 8 | ✅ 全パス |
| test-visibility-recommendations.mjs | 6 | ✅ 全パス |
| test-field.mjs（実地試験） | 13 | ✅ 全パス |
| verify-recall-migration.mjs（非破壊検証） | 93 | ✅ 全パス |
| **合計** | **201** | **✅ 全パス** |

## 4. KPI実測

| KPI | 目標 | 実測 | 判定 |
|---|---|---|---|
| 通常検索の平均返却token | 全文中心の基準比60%以上削減 | **792 → 277 tokens（−65.0%）** | ✅ 達成 |
| 必要記憶の再現率 | 95%以上 | 実地試験の全シナリオで到達（欠損0） | ✅ |
| raw memory改変 | 0件 | **0件** | ✅ |
| 履歴参照可能率 | 100% | include_history / recall_deep で100% | ✅ |
| 通常検索でsuperseded混入率 | 原則0% | **0%**（S3で実証） | ✅ |

計測方法: `node bin/measure-tokens.mjs --compare backups/token-baseline-2026-08-21T23-35-30.json`
（固定コーパス7件・代表クエリ8群・隔離ストア・同一推定方式）

## 5. 主要機能

| 機能 | 説明 |
|---|---|
| Visibility Layer | hot/warm/cold/archive。importanceと独立。`promote_memory`（reason必須） |
| Token Budget | `token_budget` / `max_results`。低関連から切り詰め、通知付き |
| Summary Layer | 決定論的抽出（LLM不使用）。source_hash不一致は無効化・自動再生成 |
| Progressive Disclosure | 検索既定=Level0-1。`full_content=true` で全文取得。`recall_deep` による深掘り |
| Usage Tracking | access_count記録。`begin_retrieval_session` + session_idで既読ペナルティ |
| Recall理由ログ | 「なぜ思い出したか」を記録。Viewer詳細画面に表示 |
| Viewer | visibilityバッジ・参照回数・想起理由の表示 |
| Visibility推奨 | `get_visibility_recommendations`。自動適用なし・根拠付き |

## 6. 既知の制限

- summary は決定論的抽出（先頭文+キー文選別）のため、LLM要約より粗い。将来差し替え可能な構造。
- visibility の自動昇降格は未実装（推奨ベースのみ）。将来の拡張候補。
- entity linking / knowledge graph は未導入（コスト対効果未実証）。

## 7. 結論

公開安定版 v1.0.0 の定義済み完了条件を満たし、記憶欠損0・raw改変0・KPI達成を確認した。**ANTmemory v1.0.0 リリース可**。
