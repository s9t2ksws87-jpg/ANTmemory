# Upgrade public 2.x to ANTmemory 4

V4 changes the runtime/storage layout. Keep an offline backup of the whole stores/ directory and runtime/workspaces.json, plus the old release. Never run both generations against one installation.

The CLI calls the older store.db format **v3** internally; its importer also handles public 2.x native databases. The public-2.x fixture verifies Raw, withdrawal, state and unmapped rows; it is not proof that every user's database is already migrated. Old source remains at tag v2.0.0 and legacy/v2/.

## Windows procedure

1. Stop MCP clients using the old installation. Back up stores/ and the workspace registry. An offline folder copy or SQLite backup API retains WAL data; copying only a live main DB file may not.
2. Extract the complete V4 ZIP into a new folder. Copy the backed-up stores/ and registry into that folder, retaining the original backup. Use another copy for rehearsal when possible.
3. Open PowerShell in the new installation and rehearse a store conversion:

```powershell
$env:ANT_MEMORY_ROOT = (Get-Location).Path
./runtime/bin/antmemory.exe migrate --rehearse --from ./stores/my-project/store.db --out-dir ./runtime/rehearsal-my-project --registry ./runtime/workspaces.json
```

Replace my-project with the actual old store name. If no registry exists, use --workspace-root C:/projects/my-project instead. Inspect the migration report and equality results. Rehearsal snapshots into a separate v4.db and retains unmapped tables; it does not replace the source DB.

4. Freeze old-format writers, convert and verify:

```powershell
./runtime/bin/antmemory.exe cutover freeze --all
./runtime/bin/antmemory.exe migrate --final --all
./runtime/bin/antmemory.exe doctor --all
```

Freeze will not call a running writer stopped. Close listed clients and retry; --confirm-stop explicitly authorizes stopping matching processes. Enumeration failures are not zero writers. A failed conversion leaves a failed candidate/report instead of declaring success.

5. Configure the new executable with args ["serve"], or the new Node bridge, and restart MCP connections. Check the route and representative Raw, history, state and UNKNOWNs.

Old stores/snapshots remain. Already-running old launchers do not gain new freeze rules merely because their files were replaced; keep old clients stopped. Never upload store files or private configuration to the public repository.

## Rollback

Stop V4 before using the old release. If no new V4 data needs retaining, restore the offline pre-upgrade backup. Otherwise use export-v3 --store my-project --out C:/recovery/store.db against the final migration snapshot, inspect its verification, and keep the V4 data/reports. Supplemental export tables retain V4-only data; goal cards and connection observations do not acquire identical 2.x behavior.

## 日本語の要点

旧クライアント停止→フォルダ全体のバックアップ→新しい配置へコピー→予行照合→凍結と最終変換→点検→MCP再接続の順です。CLI内の「v3」は公開2.xのnative DBも含む旧形式の呼称です。store.dbを消して置換する手順ではありません。停止未確認や照合失敗のまま進めず、切戻しでも両世代を同時起動しません。
