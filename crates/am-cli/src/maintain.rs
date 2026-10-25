//! 長く使うための手入れ。daemonが定期的に行い、`antmemory maintain` でも手で行える。
//!
//! - 毎日のバックアップ（SQLiteのbackup APIで、書込中でも一貫した版）。新しい方から BACKUP_KEEP 個を残す
//! - WALの checkpoint と `PRAGMA optimize`（WALが育ち続けない・索引の統計を新しくする）
//! - ログの回転（daemon.log・hook.log が育ち続けない）
//!
//! どれも記憶・イベントは変えない（DBの内容を変えるのは `Store::apply` だけ）。

use std::path::{Path, PathBuf};

use am_store::Store;

pub const BACKUP_KEEP: usize = 14;
pub const LOG_MAX_BYTES: u64 = 5 * 1024 * 1024;
pub const LOG_KEEP: usize = 3;

pub fn backup_dir(store_dir: &Path) -> PathBuf {
    store_dir.join("backups")
}

/// 今日のバックアップが無ければ取る。古いものは BACKUP_KEEP 個を超えた分だけ外す。
/// 戻り値は、取ったときのパス。
pub fn daily_backup(store_dir: &Path, store: &Store) -> anyhow::Result<Option<PathBuf>> {
    let dir = backup_dir(store_dir);
    std::fs::create_dir_all(&dir)?;
    let today = chrono::Local::now().format("%Y%m%d").to_string();
    let dst = dir.join(format!("v4-{today}.db"));
    if dst.exists() {
        prune(&dir)?;
        return Ok(None);
    }
    let tmp = dir.join(format!("v4-{today}.db.partial"));
    {
        let mut out = rusqlite::Connection::open(&tmp)?;
        let b = rusqlite::backup::Backup::new(store.reader(), &mut out)?;
        b.run_to_completion(512, std::time::Duration::from_millis(5), None)?;
    }
    // 取り切ったものだけを、完成した名前にする（途中で落ちても壊れた版を残さない）。
    std::fs::rename(&tmp, &dst)?;
    prune(&dir)?;
    Ok(Some(dst))
}

/// 新しい方から BACKUP_KEEP 個を残す（名前に日付があるので、名前の順が日付の順）。
fn prune(dir: &Path) -> anyhow::Result<()> {
    let mut olds: Vec<PathBuf> = std::fs::read_dir(dir)?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().is_some_and(|n| n.to_string_lossy().starts_with("v4-") && n.to_string_lossy().ends_with(".db")))
        .collect();
    olds.sort();
    while olds.len() > BACKUP_KEEP {
        let p = olds.remove(0);
        std::fs::remove_file(&p).ok();
    }
    Ok(())
}

/// WALを本体へ書き戻して小さくし、索引の統計を更新する。
/// WALの書き戻しの結果。読んでいる接続があると最後まで書き戻せず、次の機会へ延びる（busy）。
#[derive(Debug)]
pub struct Checkpoint {
    pub busy: bool,
    pub wal_pages: i64,
    pub done_pages: i64,
}

impl Checkpoint {
    pub fn complete(&self) -> bool {
        !self.busy && self.done_pages >= self.wal_pages
    }
}

pub fn checkpoint(store: &Store) -> anyhow::Result<Checkpoint> {
    let c = store.reader();
    let (busy, wal_pages, done_pages): (i64, i64, i64) =
        c.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    c.execute_batch("PRAGMA optimize;")?;
    Ok(Checkpoint { busy: busy != 0, wal_pages, done_pages })
}

/// 中身の点検（SQLiteの quick_check）。"ok" 以外は、その内容を返す。
pub fn quick_check(store: &Store) -> anyhow::Result<Vec<String>> {
    let mut st = store.reader().prepare("PRAGMA quick_check")?;
    let rows: Vec<String> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    Ok(rows.into_iter().filter(|r| r != "ok").collect())
}

/// ログが LOG_MAX_BYTES を超えたら、log → log.1 → log.2 … と送り、LOG_KEEP 個を残す。
pub fn rotate(path: &Path) -> bool {
    let big = std::fs::metadata(path).map(|m| m.len() > LOG_MAX_BYTES).unwrap_or(false);
    if !big {
        return false;
    }
    let name = |i: usize| PathBuf::from(format!("{}.{i}", path.display()));
    std::fs::remove_file(name(LOG_KEEP)).ok();
    for i in (1..LOG_KEEP).rev() {
        std::fs::rename(name(i), name(i + 1)).ok();
    }
    std::fs::rename(path, name(1)).is_ok()
}

/// 1つのstoreの手入れ（手動の `antmemory maintain`）。
/// 1つのstoreの手入れ（手動の `antmemory maintain`）。
/// 点検で問題が見つかれば、バックアップを取らずに（壊れた版で古い良い版を押し出さないよう）エラーで返す（独立レビュー I4）。
pub fn run_store(store_dir: &Path) -> anyhow::Result<String> {
    let store = Store::open(&store_dir.join("v4.db"), false)?;
    let problems = quick_check(&store)?;
    if !problems.is_empty() {
        anyhow::bail!("点検で問題 {}件（バックアップは取っていない）: {}", problems.len(), problems.join("; "));
    }
    let b = daily_backup(store_dir, &store)?;
    let cp = checkpoint(&store)?;
    let wal = if cp.complete() {
        format!("WAL {}ページを書き戻した", cp.done_pages)
    } else {
        format!("WALの書き戻しは途中（{}/{}ページ。読んでいる接続がある。次の手入れで続ける）", cp.done_pages, cp.wal_pages)
    };
    Ok(format!("点検 ok / バックアップ {} / {wal}", b.map(|p| p.display().to_string()).unwrap_or_else(|| "今日の分はある".into())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_once_per_day_and_rotation() {
        let d = tempfile::tempdir().unwrap();
        let store = Store::create(&d.path().join("v4.db"), "t").unwrap();
        let first = daily_backup(d.path(), &store).unwrap();
        assert!(first.as_ref().is_some_and(|p| p.is_file()));
        assert!(daily_backup(d.path(), &store).unwrap().is_none(), "同じ日に2回は取らない");
        let back = Store::open(first.as_ref().unwrap(), true).unwrap();
        assert!(quick_check(&back).unwrap().is_empty());
        drop(back);
        for i in 0..20 {
            std::fs::write(backup_dir(d.path()).join(format!("v4-2000010{i:02}.db")), "x").unwrap();
        }
        std::fs::remove_file(first.unwrap()).ok();
        daily_backup(d.path(), &store).unwrap();
        let n = std::fs::read_dir(backup_dir(d.path())).unwrap().flatten().filter(|e| e.file_name().to_string_lossy().ends_with(".db")).count();
        assert_eq!(n, BACKUP_KEEP);

        let log = d.path().join("daemon.log");
        std::fs::write(&log, vec![b'x'; (LOG_MAX_BYTES + 1) as usize]).unwrap();
        assert!(rotate(&log));
        assert!(!log.exists() && d.path().join("daemon.log.1").exists());
        assert!(!rotate(&log));
    }

    #[test]
    fn integrity_problem_fails_and_takes_no_backup() {
        let d = tempfile::tempdir().unwrap();
        {
            let store = Store::create(&d.path().join("v4.db"), "t").unwrap();
            // CHECK制約に反する行を、制約の検査を外して入れる（quick_check が問題として返す）。
            store
                .reader()
                .execute_batch("CREATE TABLE damaged(x INTEGER CHECK (x > 0)); PRAGMA ignore_check_constraints=1; INSERT INTO damaged VALUES (-1); PRAGMA ignore_check_constraints=0;")
                .unwrap();
            assert!(!quick_check(&store).unwrap().is_empty());
        }
        let e = run_store(d.path()).unwrap_err().to_string();
        assert!(e.contains("点検で問題"), "{e}");
        assert!(!backup_dir(d.path()).exists() || std::fs::read_dir(backup_dir(d.path())).unwrap().count() == 0);
    }

    #[test]
    fn checkpoint_reports_busy_reader() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("v4.db");
        let store = Store::create(&p, "t").unwrap();
        store.reader().execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);").unwrap();
        // 別の接続が読取りのtransactionを開いたままにすると、書き戻しは最後まで進まない。
        let r = rusqlite::Connection::open(&p).unwrap();
        r.execute_batch("BEGIN; SELECT count(*) FROM t;").unwrap();
        store.reader().execute_batch("INSERT INTO t VALUES (2);").unwrap();
        let cp = checkpoint(&store).unwrap();
        assert!(!cp.complete(), "{cp:?}");
        r.execute_batch("COMMIT;").unwrap();
        assert!(checkpoint(&store).unwrap().complete());
    }

}
