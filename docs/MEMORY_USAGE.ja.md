# ANTmemory 4の使い方

作業開始・以前の判断・見慣れない変更に出会った時にsearch_context(query)を使います。1回で見つかれば反復不要。必要な時だけ語を変えます。決定と理由、失敗、停止点を残し、ファイル全体の写しを大量に保存する用途にはしません。

## 保存と読取

write_context(content)だけで保存できます。題名は最初の空でない行、主体は接続元、作業空間と記録時Git情報は自動です。tags・anchors・source_ref・重要度・TTLは任意です。

検索は既定4件・約1,000推定token。top_k、token_budget、full_content、include_history、include_hidden、verboseを指定できます。スコアは候補内の相対値で、確率や正しさではありません。

read参照をそのままread_context(id)へ渡すと、どの検索から選んだかが接続を跨いで伝わります。開いてから無関係と分かったら、同じ参照へverdict="not_relevant"を付けて伝えられます。先に覚えた語や過去読取欄にも反映しますが、本文の語彙一致を禁止したりRawを消す操作にはしません。

```text
read_context(id="記憶ID@検索ID")
read_context(id="記憶ID", around=2487)
read_context(id="記憶ID", start=2487, length=400)
read_context(id="記憶ID", token_budget=600)
```

範囲はRaw内のUTF-16位置です。要約で本文を置き換えず、無指定なら全文を読みます。verbose=trueで記録時環境・取手・学習情報を取得します。現物は現在の状態を再観測し、報告と実行証拠を区別します。

## 履歴と観測

軽微訂正はcorrect_context、置換はsupersede_context、撤回はretract_context、適格な撤回・期限切れの復帰はrestore_contextで、理由が必須です。重要度はchange_importance、可視性はset_visibility、履歴はget_context_history。旧本文を消して書き直しません。

write_context(kind="unknown"/"hypothesis"/"observation", ...)はobserve_contextと同じ処理です。UNKNOWNを解決した観測をparent_idとresolves_unknown=trueで別記録にし、元を上書きしません。現行未解決はlist_unknownsで確認します。

## 札と黒板

post_goal(title)で作業を掲げ、終わったらclose_goal(id)。未完了は掲げたままで保留操作はありません。子にはparentを指定します。show_goals(id)は配下の閲覧だけ。enter=trueまたはsearch_context(goal=id)で作業先も選択します。他作業を見ただけでは保存先を変えません。trace=trueには対象IDが必要です。全体目標は対応する会話原文の引用を照合できた時だけ正式になり、不明なら候補を残します。

stateは現在の黒板です。get_state・set_state・list_stateを使い、競合を防ぐ場合はexpected_valueを付けます。持続する判断は記憶に残します。旧goal.currentは互換の通常stateで、照合済み全体目標ではありません。

## 返答量と接続先

一覧の既定は、記憶20行・約1,200token、UNKNOWN10行・約1,000、state20行・約1,000です。返ったoffsetと同じ絞込み条件で続きを開きます。札の続きにはrevisionが付き、途中更新なら先頭から読み直します。未提示を既読にしません。

概算はCJK/UTF-16式で、課金tokenや会話占有率ではありません。Raw全文は長くなる場合があります。圧縮世代を観測できるC2の同じ接続だけ自動省略でき、C0/C1は記憶を仮定しません。fresh=trueで再送。手動known_revisionsは任意で、圧縮後には持ち越しません。

接続先は、ANT_MEMORY_WORKSPACE明示、.antmemory-workspace、cwdの目印、環境の手がかり、対応するAntigravity情報、_sharedの順です。参照はUTF-8の1行で相対パスの基準はファイル位置です。無効な参照は理由を返し、作業先不明の場合は共有先を使うと短く知らせます。
