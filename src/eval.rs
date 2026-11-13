//! Retrieval quality on a known-item query set: for each query, the rank of the first paper that
//! counts as a hit. Reports Hit@1/3/10 and MRR per kind of query, and the queries that miss.

use std::collections::BTreeMap;
use std::fmt::Write;

use docsearch_core::View;

use crate::library::{Library, Query};
use crate::paper;

pub struct Case {
    pub kind: String,
    pub query: String,
    pub targets: Vec<String>,
}

/// Parses `kind<TAB>query<TAB>id|id`; lines starting with `#` and blank lines are skipped.
pub fn parse(text: &str) -> Result<Vec<Case>, String> {
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let [kind, query, ids] = cols[..] else {
            return Err(format!("line {}: expected kind<TAB>query<TAB>ids", n + 1));
        };
        let targets: Vec<String> = ids.split('|').filter_map(paper::normalize_id).collect();
        if targets.is_empty() {
            return Err(format!("line {}: no valid arXiv id", n + 1));
        }
        out.push(Case { kind: kind.trim().to_string(), query: query.trim().to_string(), targets });
    }
    Ok(out)
}

#[derive(Default)]
struct Tally {
    n: usize,
    hit1: usize,
    hit3: usize,
    hit10: usize,
    rr: f64,
}

pub fn run(lib: &Library, view: &View, cases: &[Case], k: usize, prior: Option<f32>) -> String {
    let mut per_kind: BTreeMap<&str, Tally> = BTreeMap::new();
    let mut misses = String::new();
    let mut absent = Vec::new();
    let mut total_ms = 0.0;
    for c in cases {
        if c.targets.iter().all(|t| lib.paper(t).is_none()) {
            absent.push(c.targets.join("|"));
            continue;
        }
        let a = lib.search(view, &Query { text: &c.query, k, prior, ..Query::default() });
        total_ms += a.elapsed.as_secs_f64() * 1000.0;
        let rank = a.found.iter().position(|f| c.targets.contains(&f.paper.id)).map(|r| r + 1);
        let t = per_kind.entry(&c.kind).or_default();
        t.n += 1;
        if let Some(r) = rank {
            t.hit1 += (r <= 1) as usize;
            t.hit3 += (r <= 3) as usize;
            t.hit10 += (r <= 10) as usize;
            t.rr += 1.0 / r as f64;
        }
        if rank.is_none_or(|r| r > 3) {
            let top = a.found.first().map_or("nothing".to_string(), |f| format!("[{}] {}", f.paper.id, f.paper.title));
            let r = rank.map_or("-".to_string(), |r| r.to_string());
            let _ = writeln!(misses, "  {:<4} rank {:>2}  {}  (top: {})", c.kind, r, c.query, top);
        }
    }
    let mut out = String::new();
    let _ = writeln!(out, "{:<6} {:>4} {:>7} {:>7} {:>7} {:>6}", "kind", "n", "Hit@1", "Hit@3", "Hit@10", "MRR");
    let mut all = Tally::default();
    for (kind, t) in &per_kind {
        line(&mut out, kind, t);
        all.n += t.n;
        all.hit1 += t.hit1;
        all.hit3 += t.hit3;
        all.hit10 += t.hit10;
        all.rr += t.rr;
    }
    line(&mut out, "all", &all);
    if all.n > 0 {
        let _ = writeln!(out, "mean search time {:.2} ms (k={k})", total_ms / all.n as f64);
    }
    if !misses.is_empty() {
        let _ = writeln!(out, "\nNot in the top 3:\n{misses}");
    }
    absent.sort();
    absent.dedup();
    if !absent.is_empty() {
        let _ = writeln!(out, "Skipped, not in the index: {}", absent.join(", "));
    }
    out
}

fn line(out: &mut String, kind: &str, t: &Tally) {
    let pct = |x: usize| if t.n == 0 { 0.0 } else { 100.0 * x as f64 / t.n as f64 };
    let mrr = if t.n == 0 { 0.0 } else { t.rr / t.n as f64 };
    let _ = writeln!(
        out,
        "{:<6} {:>4} {:>6.0}% {:>6.0}% {:>6.0}% {:>6.3}",
        kind,
        t.n,
        pct(t.hit1),
        pct(t.hit3),
        pct(t.hit10),
        mrr
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cases() {
        let cases = parse("# comment\n\nname\tattention is all you need\tarXiv:1706.03762v7|2106.09685\n").unwrap();
        assert_eq!(cases.len(), 1);
        assert_eq!(cases[0].kind, "name");
        assert_eq!(cases[0].targets, ["1706.03762", "2106.09685"]);
        assert!(parse("name\tno ids\tnope\n").is_err());
        assert!(parse("only two\tcolumns\n").is_err());
    }

    #[test]
    fn the_shipped_set_parses() {
        let cases = parse(include_str!("../eval/known-items.tsv")).unwrap();
        assert!(cases.len() >= 60, "{}", cases.len());
    }
}
