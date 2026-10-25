//! v3のstoreに保存されたvectorと、v4で計算したvectorを比べる。
//! 使い方: compare_v3 <ANTmemory root> <store.db> [件数]
use am_embed::{dot, from_blob, prepare_text, Embedder};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let root = std::path::PathBuf::from(&args[1]);
    let db = rusqlite::Connection::open_with_flags(&args[2], rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let n: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(50);
    let t0 = std::time::Instant::now();
    let e = Embedder::load(&am_embed::v3_model_dir(&root).expect("v3の同値検査にはMiniLMが必要")).unwrap();
    eprintln!("load {:?}", t0.elapsed());
    let mut st = db.prepare("SELECT id, topic, tags, content, embedding FROM chunks WHERE embedding IS NOT NULL ORDER BY random() LIMIT ?1").unwrap();
    let rows: Vec<(String, String, String, String, Vec<u8>)> = st
        .query_map([n as i64], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))
        .unwrap()
        .map(|r| r.unwrap())
        .collect();
    let mut sims = Vec::new();
    let t1 = std::time::Instant::now();
    for (id, topic, tags, content, blob) in &rows {
        let tags: Vec<String> = serde_json_lite(tags);
        let v4 = e.embed(&prepare_text(topic, &tags, content)).unwrap();
        let v3 = from_blob(blob);
        let s = dot(&v3, &v4);
        if s < 0.999 {
            println!("low {id} cos={s:.5} len={}", content.chars().count());
        }
        sims.push(s);
    }
    sims.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "n={} min={:.6} median={:.6} max={:.6} per_item={:?}",
        sims.len(), sims[0], sims[sims.len() / 2], sims[sims.len() - 1],
        t1.elapsed() / sims.len().max(1) as u32
    );
}

// tagsはJSONの文字列配列。依存を増やさないよう、ここでは簡単に読む。
fn serde_json_lite(s: &str) -> Vec<String> {
    let s = s.trim().trim_start_matches('[').trim_end_matches(']');
    if s.trim().is_empty() {
        return vec![];
    }
    s.split("\",").map(|x| x.trim().trim_matches('"').replace("\\\"", "\"")).collect()
}
