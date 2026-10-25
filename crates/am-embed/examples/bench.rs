use am_embed::Embedder;
fn main() {
    let root = std::path::PathBuf::from(std::env::args().nth(1).unwrap());
    let e = Embedder::load(&am_embed::default_model_dir(&root)).unwrap();
    for n in [5usize, 40, 100, 220, 480] {
        let text = "デュエマの山札 ".repeat(n / 3 + 1);
        let t = std::time::Instant::now();
        e.embed(&text).unwrap();
        let first = t.elapsed();
        let t = std::time::Instant::now();
        for _ in 0..3 { e.embed(&text).unwrap(); }
        println!("chars={} first={:?} steady={:?}", text.chars().count(), first, t.elapsed() / 3);
    }
}
