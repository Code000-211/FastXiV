//! Text for a model to read: compact, every paper with its arXiv id so it can be cited.

use std::fmt::Write;

use crate::library::Answer;
use crate::paper::Paper;

/// Abstracts of the first `detail` results are shown up to this many characters.
const ABSTRACT_CHARS: usize = 700;
const AUTHORS_SHOWN: usize = 4;

fn cut(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    if let Some(space) = out.rfind(' ') {
        out.truncate(space);
    }
    out.push_str(" …");
    out
}

fn month(p: &Paper) -> String {
    p.month().map_or_else(|| "?".to_string(), |(y, m)| format!("{y}-{m:02}"))
}

fn authors(p: &Paper, shown: usize) -> String {
    let total = p.authors.len() + p.more_authors;
    let mut s = p.authors.iter().take(shown).cloned().collect::<Vec<_>>().join(", ");
    if total > shown {
        let _ = write!(s, " et al. ({total} authors)");
    }
    s
}

pub fn answer(a: &Answer, query: &str, detail: usize) -> String {
    let mut out = String::new();
    let ms = a.elapsed.as_secs_f64() * 1000.0;
    if a.found.is_empty() {
        let _ = writeln!(out, "No papers found for \"{query}\" ({ms:.1} ms). Try other words, or fewer filters.");
        return out;
    }
    let n = a.found.len();
    let _ = write!(out, "{n} paper{} for \"{query}\" ({ms:.1} ms)", if n == 1 { "" } else { "s" });
    if !a.entities.is_empty() {
        let shown: Vec<&str> = a.entities.iter().take(8).map(String::as_str).collect();
        let more = if a.entities.len() > 8 { ", …" } else { "" };
        let _ = write!(out, "; names linked in the graph: {}{more}", shown.join(", "));
    }
    let _ = writeln!(out, ":");
    for (i, f) in a.found.iter().enumerate() {
        let p = &f.paper;
        let _ = writeln!(out, "\n{}. [{}] {}", i + 1, p.id, p.title);
        let cited = f.citations.map_or(String::new(), |n| format!(" · {n} citation{}", if n == 1 { "" } else { "s" }));
        let _ = writeln!(out, "   {} · {}{cited} · {}", month(p), p.categories.join(" "), authors(p, AUTHORS_SHOWN));
        if i < detail {
            if let Some(c) = &p.comments {
                let _ = writeln!(out, "   Comments: {}", cut(c, 160));
            }
            let _ = writeln!(out, "   {}", cut(&p.abstract_, ABSTRACT_CHARS));
        }
    }
    out
}

pub fn paper(p: &Paper) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# {}\n", p.title);
    let _ = write!(out, "arXiv: {} · submitted {}", p.id, month(p));
    if !p.updated.is_empty() {
        let _ = write!(out, " · updated {}", p.updated);
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Authors: {}", authors(p, usize::MAX));
    let _ = writeln!(out, "Categories: {}", p.categories.join(" "));
    for (key, value) in [("Comments", &p.comments), ("Journal", &p.journal), ("DOI", &p.doi)] {
        if let Some(v) = value {
            let _ = writeln!(out, "{key}: {v}");
        }
    }
    let _ = writeln!(
        out,
        "Links: https://arxiv.org/abs/{id} · https://arxiv.org/pdf/{id} · https://www.alphaxiv.org/abs/{id}",
        id = p.id
    );
    let _ = writeln!(out, "\n{}", p.abstract_);
    out
}
