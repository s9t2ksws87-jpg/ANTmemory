//! 文の埋め込み。対応モデルの入力・前処理を明示してONNX Runtime (ort) で動かす。
//!
//! tokenizerでトークナイズし、ONNX Runtimeで推論後、
//! 最後の隠れ層（last_hidden_state）をattention maskで平均（mean pooling）し、L2で正規化する。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ort::session::builder::GraphOptimizationLevel;
use ort::session::Session;
use ort::value::{Tensor, TensorElementType, ValueType};
use sha2::{Digest, Sha256};
use tokenizers::{Tokenizer, TruncationParams};

// ort rc.15ではEnvironmentを一度しか作れない。Sessionより長く保持し、複数loadでも共有する。
static ENVIRONMENT: Mutex<Option<ort::environment::Environment>> = Mutex::new(None);

#[derive(Debug, Clone, Copy)]
struct ModelSpec {
    name: &'static str,
    dim: usize,
    max_tokens: usize,
    query_prefix: &'static str,
    document_prefix: &'static str,
}

impl ModelSpec {
    fn for_dir(dir: &Path) -> Result<Self, EmbedError> {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        let (name, dim, max_tokens, query_prefix, document_prefix) = match name {
            "ruri-v3-70m" => ("cl-nagoya/ruri-v3-70m", 384, 8192, "検索クエリ: ", "検索文書: "),
            "ruri-v3-30m" => ("cl-nagoya/ruri-v3-30m", 256, 8192, "検索クエリ: ", "検索文書: "),
            "all-MiniLM-L6-v2" => ("Xenova/all-MiniLM-L6-v2", 384, 512, "", ""),
            _ => return Err(EmbedError::Unsupported(format!("未対応のモデルフォルダ: {}", dir.display()))),
        };
        Ok(Self { name, dim, max_tokens, query_prefix, document_prefix })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EmbedError {
    #[error("モデルのファイルが無い: {0}")]
    Missing(String),
    #[error("onnxruntime.dll が見つからない: {0}")]
    MissingDll(String),
    #[error("tokenizer: {0}")]
    Tokenizer(String),
    #[error("onnx: {0}")]
    Onnx(String),
    #[error("モデルの契約が合わない: {0}")]
    Unsupported(String),
}

pub struct Embedder {
    pub model_dir: PathBuf,
    tokenizer: Tokenizer,
    session: Mutex<Session>,
    spec: ModelSpec,
    model_id: String,
    output_name: String,
    has_token_type_ids: bool,
}

const CANDIDATE_MODELS: [&str; 2] = ["models/ruri-v3-70m", "models/all-MiniLM-L6-v2"];

const V3_CACHE: [&str; 2] = [
    "legacy/v3/server/node_modules/@huggingface/transformers/.cache/Xenova/all-MiniLM-L6-v2",
    "server/node_modules/@huggingface/transformers/.cache/Xenova/all-MiniLM-L6-v2",
];

/// onnxruntime.dll を探索する。
pub fn find_onnxruntime_dll(antmemory_root: &Path) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("ORT_DYLIB_PATH") {
        let pb = PathBuf::from(p);
        if pb.is_file() {
            return Some(pb);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("onnxruntime.dll");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    let local = antmemory_root.join("runtime/bin/onnxruntime.dll");
    if local.is_file() {
        return Some(local);
    }
    let target = antmemory_root.join("target/debug/onnxruntime.dll");
    if target.is_file() {
        return Some(target);
    }
    // target/releaseから別rootへinstallする場合も、配布元rootのDLLを見つける。
    if let Ok(exe) = std::env::current_exe() {
        for dir in exe.ancestors().skip(1) {
            let p = dir.join("runtime/bin/onnxruntime.dll");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}

pub fn default_model_dir(antmemory_root: &Path) -> PathBuf {
    for c in CANDIDATE_MODELS {
        let p = antmemory_root.join(c);
        if p.join("onnx/model.onnx").is_file() && p.join("tokenizer.json").is_file() {
            return p;
        }
    }
    V3_CACHE.iter().map(|c| antmemory_root.join(c)).find(|d| d.is_dir()).unwrap_or_else(|| antmemory_root.join(CANDIDATE_MODELS[0]))
}

/// 記憶の本文準備：topic、tags、本文（UTF-16で先頭800字）をつなぐ。
pub fn prepare_text(topic: &str, tags: &[String], content: &str) -> String {
    let units: Vec<u16> = content.encode_utf16().collect();
    let truncated = if units.len() > 800 { String::from_utf16_lossy(&units[..800]) } else { content.to_string() };
    format!("{} {} {}", topic, tags.join(" "), truncated).trim().to_string()
}

impl Embedder {
    pub fn load(model_dir: &Path) -> Result<Embedder, EmbedError> {
        let model_path = model_dir.join("onnx/model.onnx");
        let tok_path = model_dir.join("tokenizer.json");
        for p in [&model_path, &tok_path] {
            if !p.is_file() {
                return Err(EmbedError::Missing(p.display().to_string()));
            }
        }
        let spec = ModelSpec::for_dir(model_dir)?;

        let antmemory_root = model_dir.ancestors().find(|p| p.join("Cargo.toml").is_file() || p.join("runtime").is_dir()).unwrap_or(model_dir);
        let dll_path = find_onnxruntime_dll(antmemory_root)
            .ok_or_else(|| EmbedError::MissingDll("onnxruntime.dll が見つかりません。runtime/bin または ORT_DYLIB_PATH を確認してください。".into()))?;

        let mut tokenizer = Tokenizer::from_file(&tok_path).map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        tokenizer
            .with_truncation(Some(TruncationParams { max_length: spec.max_tokens, ..Default::default() }))
            .map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        tokenizer.with_padding(None);

        let env = {
            let mut current = ENVIRONMENT.lock().map_err(|_| EmbedError::Onnx("environment lock poisoned".into()))?;
            if current.is_none() {
                *current = Some(
                    ort::init_from(&dll_path)
                        .map_err(|e| EmbedError::Onnx(format!("ort init: {e}")))?
                        .with_name("antmemory")
                        .build()
                        .map_err(|e| EmbedError::Onnx(format!("ort env build: {e}")))?,
                );
            }
            current.as_ref().expect("initialized above").clone()
        };

        let session = Session::builder(&env)
            .map_err(|e| EmbedError::Onnx(format!("session builder: {e}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| EmbedError::Onnx(format!("opt level: {e}")))?
            .with_intra_threads(4)
            .map_err(|e| EmbedError::Onnx(format!("intra threads: {e}")))?
            .commit_from_file(&model_path)
            .map_err(|e| EmbedError::Onnx(format!("session commit: {e}")))?;

        let mut has_token_type_ids = false;
        for input in session.inputs() {
            if !matches!(input.name(), "input_ids" | "attention_mask" | "token_type_ids")
                || !matches!(input.dtype(), ValueType::Tensor { ty: TensorElementType::Int64, shape, .. } if shape.len() == 2)
            {
                return Err(EmbedError::Unsupported(format!("入力 {}: {:?}", input.name(), input.dtype())));
            }
            if input.name() == "token_type_ids" {
                has_token_type_ids = true;
            }
        }
        for required in ["input_ids", "attention_mask"] {
            if !session.inputs().iter().any(|i| i.name() == required) {
                return Err(EmbedError::Unsupported(format!("入力 {required} が無い")));
            }
        }
        let output = session
            .outputs()
            .iter()
            .find(|o| matches!(o.name(), "last_hidden_state" | "token_embeddings"))
            .ok_or_else(|| EmbedError::Unsupported("mean pooling用のtoken出力が無い".into()))?;
        if !matches!(output.dtype(), ValueType::Tensor { ty: TensorElementType::Float32, shape, .. }
            if shape.len() == 3 && shape[2] == spec.dim as i64)
        {
            return Err(EmbedError::Unsupported(format!("出力 {}: {:?}", output.name(), output.dtype())));
        }
        let output_name = output.name().to_string();
        // 同じフォルダ名でも重み・tokenizer・前処理が変われば、既存vectorと混ぜず作り直す。
        let mut fingerprint = Sha256::new();
        for path in [&model_path, &tok_path] {
            let mut file = std::fs::File::open(path).map_err(|e| EmbedError::Missing(format!("{}: {e}", path.display())))?;
            fingerprint.update(file.metadata().map_err(|e| EmbedError::Missing(e.to_string()))?.len().to_le_bytes());
            let mut buffer = [0u8; 65536];
            loop {
                let n = file.read(&mut buffer).map_err(|e| EmbedError::Missing(e.to_string()))?;
                if n == 0 {
                    break;
                }
                fingerprint.update(&buffer[..n]);
            }
        }
        let model_id = format!("{}:mean:l2:max{}:retrieval-v1:sha256={}", spec.name, spec.max_tokens, hex::encode(fingerprint.finalize()));

        Ok(Embedder { model_dir: model_dir.to_path_buf(), tokenizer, session: Mutex::new(session), spec, model_id, output_name, has_token_type_ids })
    }

    pub fn embed_query(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        self.embed(&format!("{}{text}", self.spec.query_prefix))
    }

    pub fn embed_document(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        self.embed(&format!("{}{text}", self.spec.document_prefix))
    }

    pub fn embed(&self, text: &str) -> Result<Vec<f32>, EmbedError> {
        let enc = self.tokenizer.encode(text, true).map_err(|e| EmbedError::Tokenizer(e.to_string()))?;
        let ids: Vec<i64> = enc.get_ids().iter().map(|&x| x as i64).collect();
        let mask: Vec<i64> = enc.get_attention_mask().iter().map(|&x| x as i64).collect();
        let seq_len = ids.len();

        let shape = [1, seq_len];
        let ids_tensor = Tensor::from_array((shape, ids)).map_err(|e| EmbedError::Onnx(e.to_string()))?;
        let mask_tensor = Tensor::from_array((shape, mask.clone())).map_err(|e| EmbedError::Onnx(e.to_string()))?;

        let mut session = self.session.lock().map_err(|_| EmbedError::Onnx("session lock poisoned".into()))?;
        let outputs = if self.has_token_type_ids {
            let types = vec![0i64; seq_len];
            let types_tensor = Tensor::from_array((shape, types)).map_err(|e| EmbedError::Onnx(e.to_string()))?;
            session.run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
                "token_type_ids" => types_tensor,
            ])
        } else {
            session.run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
            ])
        }
        .map_err(|e| EmbedError::Onnx(format!("inference error: {e}")))?;

        let output = outputs.get(self.output_name.as_str()).ok_or_else(|| EmbedError::Onnx("token出力が無い".into()))?;
        let (out_shape, data) = output.try_extract_tensor::<f32>().map_err(|e| EmbedError::Onnx(e.to_string()))?;
        mean_pool(out_shape, data, &mask, self.spec.dim)
    }

    pub fn dim(&self) -> usize {
        self.spec.dim
    }

    pub fn model_id(&self) -> &str {
        &self.model_id
    }
}

fn mean_pool(shape: &[i64], data: &[f32], mask: &[i64], dim: usize) -> Result<Vec<f32>, EmbedError> {
    let seq_len = mask.len();
    if shape != [1, seq_len as i64, dim as i64] || seq_len.checked_mul(dim) != Some(data.len()) {
        return Err(EmbedError::Unsupported(format!("token出力のshapeが違う: {shape:?}")));
    }

    // Mean pooling over tokens where attention_mask == 1
    let mut sum = vec![0f32; dim];
    let mut count = 0f32;
    for t in 0..seq_len {
        if mask[t] == 1 {
            count += 1.0;
            for d in 0..dim {
                sum[d] += data[t * dim + d];
            }
        }
    }
    if count == 0.0 {
        return Err(EmbedError::Onnx("attention maskが空".into()));
    }
    let mut mean: Vec<f32> = sum.iter().map(|&x| x / count).collect();

    // L2 normalization
    let norm = mean.iter().map(|&x| x * x).sum::<f32>().sqrt();
    if !norm.is_finite() || norm < 1e-12 {
        return Err(EmbedError::Onnx("vectorの値が有限でないか、長さが0".into()));
    }
    for x in &mut mean {
        *x /= norm;
    }

    Ok(mean)
}

pub fn to_blob(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

pub fn from_blob(b: &[u8]) -> Vec<f32> {
    b.as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// モデルの場所。ANT_MEMORY_MODEL_DIR があればそれ。無ければ、
/// models/ruri-v3-70m、models/all-MiniLM-L6-v2、v3のcache の順に探す。
pub fn model_dir(antmemory_root: &Path) -> PathBuf {
    if let Some(p) = std::env::var_os("ANT_MEMORY_MODEL_DIR") {
        return PathBuf::from(p);
    }
    let has = |d: &Path| d.join("onnx/model.onnx").is_file() && d.join("tokenizer.json").is_file();
    for c in CANDIDATE_MODELS {
        let local = antmemory_root.join(c);
        if has(&local) {
            return local;
        }
    }
    let mut dir = Some(antmemory_root.to_path_buf());
    while let Some(d) = dir {
        for c in CANDIDATE_MODELS {
            let p = d.join(c);
            if has(&p) {
                return p;
            }
        }
        if let Some(c) = V3_CACHE.iter().map(|c| d.join(c)).find(|c| has(c)) {
            return c;
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    default_model_dir(antmemory_root)
}

/// v3との同値検査では、現在のruriではなくv3と同じMiniLMを使う。
pub fn v3_model_dir(root: &Path) -> Option<PathBuf> {
    let mut dir = Some(root.to_path_buf());
    while let Some(d) = dir {
        for c in std::iter::once("models/all-MiniLM-L6-v2").chain(V3_CACHE) {
            let p = d.join(c);
            if p.join("onnx/model.onnx").is_file() && p.join("tokenizer.json").is_file() {
                return Some(p);
            }
        }
        dir = d.parent().map(Path::to_path_buf);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_contract_is_per_model_and_not_a_parent_path_substring() {
        let mini = ModelSpec::for_dir(Path::new("ruri-v3-70m/models/all-MiniLM-L6-v2")).unwrap();
        assert_eq!(mini.max_tokens, 512);
        assert_eq!(mini.query_prefix, "");
        let ruri = ModelSpec::for_dir(Path::new("models/ruri-v3-70m")).unwrap();
        assert_eq!((ruri.dim, ruri.max_tokens), (384, 8192));
        assert_eq!((ruri.query_prefix, ruri.document_prefix), ("検索クエリ: ", "検索文書: "));
        assert_eq!(ModelSpec::for_dir(Path::new("models/ruri-v3-30m")).unwrap().dim, 256);
        assert!(ModelSpec::for_dir(Path::new("models/unknown")).is_err());
    }

    #[test]
    fn pooling_uses_mask_and_rejects_unexpected_or_invalid_output_without_panicking() {
        assert_eq!(mean_pool(&[1, 2, 2], &[3., 4., 999., 999.], &[1, 0], 2).unwrap(), vec![0.6, 0.8]);
        assert!(mean_pool(&[1, 2], &[3., 4.], &[1], 2).is_err());
        assert!(mean_pool(&[1, 1, 2], &[3.], &[1], 2).is_err());
        assert!(mean_pool(&[1, 1, 2], &[f32::NAN, 4.], &[1], 2).is_err());
        assert!(mean_pool(&[1, 1, 2], &[0., 0.], &[1], 2).is_err());
        assert!(mean_pool(&[1, 1, 2], &[3., 4.], &[0], 2).is_err());
    }

    /// 実モデルとDLLを置いた開発環境で明示して実行する。欠けていたら成功にせず失敗する。
    #[test]
    #[ignore = "needs both downloaded models and onnxruntime.dll"]
    fn installed_models_long_input_prefixes_and_repeated_load() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap().parent().unwrap();
        let ruri = Embedder::load(&root.join("models/ruri-v3-70m")).unwrap();
        let mini = Embedder::load(&root.join("models/all-MiniLM-L6-v2")).unwrap();
        let again = Embedder::load(&root.join("models/ruri-v3-70m")).unwrap();
        assert_eq!(again.model_id(), ruri.model_id());
        assert_ne!(mini.model_id(), ruri.model_id());
        for e in [&ruri, &mini] {
            let v = e.embed_document(&"memory ".repeat(700)).unwrap();
            assert_eq!(v.len(), e.dim());
            assert!((dot(&v, &v) - 1.0).abs() < 1e-5);
        }
        let text = "別のAIの作業目的と停止点を思い出したい";
        assert_eq!(ruri.embed_query(text).unwrap(), ruri.embed(&format!("検索クエリ: {text}")).unwrap());
        assert_eq!(ruri.embed_document(text).unwrap(), ruri.embed(&format!("検索文書: {text}")).unwrap());
        assert_ne!(ruri.embed_query(text).unwrap(), ruri.embed_document(text).unwrap());
    }
}
