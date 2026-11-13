//! Citation counts from Semantic Scholar, as a ranking prior: a famous paper should outrank the
//! papers that reuse its title words ("SDS-LoRA" against LoRA). Semantic Scholar merges a preprint
//! with its published version, so arXiv ids get the citations of both.
//!
//! Stored as `citations.tsv` (`id<TAB>count`) in the data folder; ids Semantic Scholar does not
//! know are stored with an empty count, so `--missing` does not ask for them again.

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};

use crate::paper;

const BATCH_URL: &str = "https://api.semanticscholar.org/graph/v1/paper/batch?fields=citationCount";
/// The batch endpoint takes up to 500 ids per request.
const BATCH: usize = 500;

#[derive(Default)]
pub struct Citations {
    counts: HashMap<String, Option<u32>>,
}

impl Citations {
    pub fn load(path: &Path) -> Citations {
        let mut counts = HashMap::new();
        if let Ok(text) = std::fs::read_to_string(path) {
            for line in text.lines() {
                if let Some((id, n)) = line.split_once('\t') {
                    counts.insert(id.to_string(), n.trim().parse().ok());
                }
            }
        }
        Citations { counts }
    }

    pub fn get(&self, id: &str) -> Option<u32> {
        self.counts.get(id).copied().flatten()
    }

    pub fn is_empty(&self) -> bool {
        self.counts.is_empty()
    }

    pub fn len(&self) -> usize {
        self.counts.len()
    }

    fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut ids: Vec<&String> = self.counts.keys().collect();
        ids.sort();
        let mut out = String::with_capacity(ids.len() * 20);
        for id in ids {
            out.push_str(id);
            out.push('\t');
            if let Some(n) = self.counts[id] {
                out.push_str(&n.to_string());
            }
            out.push('\n');
        }
        let tmp = path.with_extension(format!("tmp{}", std::process::id()));
        std::fs::write(&tmp, out)?;
        std::fs::rename(&tmp, path)
    }
}

pub fn path(data: &Path) -> PathBuf {
    data.join("citations.tsv")
}

/// The ids of every paper in the corpus, from the file names.
fn corpus_ids(corpus: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let Ok(months) = std::fs::read_dir(corpus) else { return out };
    for month in months.flatten() {
        let Ok(files) = std::fs::read_dir(month.path()) else { continue };
        for f in files.flatten() {
            let p = f.path();
            if p.extension().is_some_and(|e| e == "md") {
                if let Some(id) = paper::id_of_path(&p) {
                    out.push(id);
                }
            }
        }
    }
    out.sort();
    out
}

/// Asks Semantic Scholar for one batch; the answer lists the papers in the order asked, `null`
/// for ids it does not know.
fn fetch_batch(agent: &ureq::Agent, ids: &[String], api_key: Option<&str>) -> Result<Vec<Option<u32>>, String> {
    let body = json!({ "ids": ids.iter().map(|id| format!("ARXIV:{id}")).collect::<Vec<_>>() }).to_string();
    let mut backoff = Duration::from_secs(5);
    let mut last = String::new();
    for _ in 0..10 {
        let mut req = agent.post(BATCH_URL).set("Content-Type", "application/json");
        if let Some(k) = api_key {
            req = req.set("x-api-key", k);
        }
        match req.send_string(&body) {
            Ok(resp) => {
                let mut text = String::new();
                resp.into_reader().read_to_string(&mut text).map_err(|e| e.to_string())?;
                let v: Value = serde_json::from_str(&text).map_err(|e| format!("bad JSON: {e}"))?;
                let arr = v.as_array().ok_or("expected a JSON array")?;
                if arr.len() != ids.len() {
                    return Err(format!("asked for {} papers, got {}", ids.len(), arr.len()));
                }
                return Ok(arr
                    .iter()
                    .map(|p| p.get("citationCount").and_then(Value::as_u64).map(|n| n as u32))
                    .collect());
            }
            Err(ureq::Error::Status(code, _)) if code == 429 || code >= 500 => last = format!("HTTP {code}"),
            Err(ureq::Error::Status(code, resp)) => {
                let detail = resp.into_string().unwrap_or_default();
                return Err(format!("HTTP {code}: {}", detail.chars().take(300).collect::<String>()));
            }
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(120));
    }
    Err(format!("giving up after retries: {last}"))
}

/// Fetches citation counts for the corpus (only ids not in the file yet with `missing_only`),
/// saving every few batches so an interrupted run loses little.
pub fn fetch(
    data: &Path,
    missing_only: bool,
    delay: Duration,
    api_key: Option<&str>,
) -> Result<(usize, usize), String> {
    let file = path(data);
    let mut cites = Citations::load(&file);
    let mut ids = corpus_ids(&data.join("corpus"));
    if missing_only {
        ids.retain(|id| !cites.counts.contains_key(id));
    }
    let agent = ureq::AgentBuilder::new()
        .try_proxy_from_env(true)
        .timeout_connect(Duration::from_secs(30))
        .timeout(Duration::from_secs(120))
        .user_agent(concat!("fastxiv/", env!("CARGO_PKG_VERSION")))
        .build();
    let (mut known, mut asked) = (0usize, 0usize);
    let batches = ids.len().div_ceil(BATCH);
    let t0 = SystemTime::now();
    for (i, chunk) in ids.chunks(BATCH).enumerate() {
        let counts = fetch_batch(&agent, chunk, api_key)?;
        for (id, n) in chunk.iter().zip(counts) {
            known += n.is_some() as usize;
            cites.counts.insert(id.clone(), n);
        }
        asked += chunk.len();
        if i % 20 == 19 || i + 1 == batches {
            cites.save(&file).map_err(|e| format!("{}: {e}", file.display()))?;
            let mins = t0.elapsed().map_or(0.0, |d| d.as_secs_f64() / 60.0);
            eprintln!(
                "citations: {}/{} batches, {known}/{asked} papers known to Semantic Scholar ({mins:.1} min)",
                i + 1,
                batches
            );
        }
        std::thread::sleep(delay);
    }
    if batches == 0 {
        cites.save(&file).map_err(|e| e.to_string())?;
    }
    Ok((known, asked))
}

/// Months since 1990 for today, from the system clock.
fn months_now() -> f64 {
    let secs = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0.0, |d| d.as_secs_f64());
    // 1970-01 is month 0 of the Unix calendar; arXiv ids count from 1991.
    secs / (365.2425 * 86400.0 / 12.0) - 20.0 * 12.0
}

/// The prior a paper adds to its normalized text score: `weight` × log10(1 + citations per year),
/// with at least a year of age so a month-old paper with two citations is not a star.
pub fn prior(weight: f32, citations: Option<u32>, id: &str) -> f32 {
    let Some(c) = citations else { return 0.0 };
    let Some((y, m)) = paper::id_month(id) else { return 0.0 };
    let paper_months = (y as f64 - 1990.0) * 12.0 + (m as f64 - 1.0);
    let years = ((months_now() - paper_months) / 12.0).max(1.0);
    weight * (1.0 + c as f64 / years).log10() as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saves_and_loads_unknown_ids_too() {
        let dir = std::env::temp_dir().join(format!("fastxiv-cites-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut c = Citations::default();
        c.counts.insert("1706.03762".into(), Some(202845));
        c.counts.insert("2611.99999".into(), None);
        c.save(&path(&dir)).unwrap();
        let back = Citations::load(&path(&dir));
        assert_eq!(back.get("1706.03762"), Some(202845));
        assert_eq!(back.get("2611.99999"), None);
        assert_eq!(back.len(), 2, "unknown ids are remembered");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn prior_favors_cited_papers_per_year() {
        let classic = prior(0.1, Some(200_000), "1706.03762");
        let fresh = prior(0.1, Some(2), "2606.16454");
        assert!(classic > 0.4 && fresh < 0.05, "{classic} {fresh}");
        assert_eq!(prior(0.1, None, "1706.03762"), 0.0);
        assert_eq!(prior(0.0, Some(1000), "1706.03762"), 0.0);
    }
}
