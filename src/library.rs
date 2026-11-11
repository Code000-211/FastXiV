//! The searchable library: the corpus folder indexed by docsearch's engine (BM25 over each paper,
//! plus a graph of papers and the names they mention, walked with personalized PageRank).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use docsearch_core::{Config, Engine, SearchOpts, View};

use crate::paper::{self, Paper};

pub struct Library {
    engine: Arc<Engine>,
    corpus: PathBuf,
}

#[derive(Default)]
pub struct Query<'a> {
    pub text: &'a str,
    pub k: usize,
    /// Submitted in or after this month.
    pub since: Option<(u16, u8)>,
    /// Submitted in or before this month.
    pub until: Option<(u16, u8)>,
    /// At least one of these categories (empty: any).
    pub categories: &'a [String],
    pub exclude: Option<&'a str>,
}

pub struct Found {
    pub paper: Paper,
}

pub struct Answer {
    pub found: Vec<Found>,
    pub elapsed: Duration,
    /// Names from the query the index knows (they seed the graph walk).
    pub entities: Vec<String>,
}

/// Parses `2024`, `2024-05` or `2024-05-17` (the day is ignored: ids carry months).
pub fn parse_month(s: &str) -> Option<(u16, u8)> {
    let mut parts = s.trim().split('-');
    let year: u16 = parts.next()?.parse().ok()?;
    let month: u8 = match parts.next() {
        Some(m) => m.parse().ok()?,
        None => 0,
    };
    ((1991..=2100).contains(&year) && month <= 12).then_some((year, month))
}

impl Library {
    /// Opens the library in `data` (`corpus/` holds the papers, `index/` the saved index) and starts
    /// loading or building its index in the background. Call before starting other threads: it sets
    /// docsearch's cache directory in the environment.
    pub fn open(data: &Path) -> Library {
        let corpus = data.join("corpus");
        std::fs::create_dir_all(&corpus).ok();
        std::env::set_var("DOCSEARCH_CACHE_DIR", data.join("index"));
        let cfg = Config {
            roots: vec![corpus.clone()],
            extensions: vec!["md".to_string()],
            max_file_bytes: 1 << 20,
            max_files: 50_000_000,
            extra_entities: None,
        };
        Library { engine: Engine::start(cfg), corpus }
    }

    /// The current index, waiting up to `wait` for the first one.
    pub fn view(&self, wait: Duration) -> Option<Arc<View>> {
        self.engine.index(wait)
    }

    pub fn building(&self) -> bool {
        self.engine.building()
    }

    /// Looks for added, changed or removed papers in the background and updates the index.
    pub fn refresh(&self) {
        self.engine.maybe_refresh();
    }

    pub fn paper(&self, id: &str) -> Option<Paper> {
        let id = paper::normalize_id(id)?;
        let text = std::fs::read_to_string(self.corpus.join(paper::rel_path(&id))).ok()?;
        Some(Paper::from_markdown(&id, &text))
    }

    pub fn search(&self, view: &View, q: &Query) -> Answer {
        let t0 = Instant::now();
        let filtered = q.since.is_some() || q.until.is_some() || !q.categories.is_empty() || q.exclude.is_some();
        let fetch = if filtered { (q.k * 20).clamp(50, 2000) } else { q.k };
        let opts = SearchOpts { k: fetch, path_prefix: None, max_per_file: 1, graph: true };
        let results = view.search(q.text, &opts);
        let mut found = Vec::with_capacity(q.k);
        for hit in &results.hits {
            if found.len() >= q.k {
                break;
            }
            let Some(id) = paper::id_of_path(hit.path) else { continue };
            if q.exclude == Some(id.as_str()) {
                continue;
            }
            if q.since.is_some() || q.until.is_some() {
                let Some(m) = paper::id_month(&id) else { continue };
                if q.since.is_some_and(|s| m < s) || q.until.is_some_and(|u| u.1 == 0 && m.0 > u.0 || u.1 > 0 && m > u)
                {
                    continue;
                }
            }
            let Ok(text) = std::fs::read_to_string(hit.path) else { continue };
            let p = Paper::from_markdown(&id, &text);
            if !q.categories.is_empty() && !p.categories.iter().any(|c| q.categories.contains(c)) {
                continue;
            }
            found.push(Found { paper: p });
        }
        Answer { found, elapsed: t0.elapsed(), entities: results.entities.iter().map(|e| e.to_string()).collect() }
    }

    /// Papers like `p`: its title and abstract as the query. The graph walk starts from the names
    /// they share (models, datasets, methods), so papers about the same things rank first.
    pub fn related(&self, view: &View, p: &Paper, k: usize) -> Answer {
        let mut text = format!("{}. {}", p.title, p.abstract_);
        if text.len() > 1200 {
            let cut = (0..=1200).rev().find(|&i| text.is_char_boundary(i)).unwrap_or(0);
            text.truncate(cut);
        }
        self.search(view, &Query { text: &text, k, exclude: Some(&p.id), ..Query::default() })
    }
}
