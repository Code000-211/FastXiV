//! A paper's metadata and how it is stored: one Markdown file per paper, the title as its heading,
//! so docsearch indexes each paper as one section (its heading terms count extra in BM25).
//!
//! ```text
//! # Attention Is All You Need
//!
//! Authors: Ashish Vaswani, Noam Shazeer, ...
//! Categories: cs.CL cs.LG
//! Updated: 2023-08-02
//! Comments: 15 pages, 5 figures
//!
//! The dominant sequence transduction models are ...
//! ```
//!
//! The arXiv id is the file name (`corpus/1706/1706.03762.md`, old style `corpus/0601/cs_0601001.md`),
//! not part of the text: a unique id in every file would add one useless entity per paper to the graph.

use std::path::{Path, PathBuf};

/// More authors than this are cut to the first ones plus a count: a collaboration with 3,000 authors
/// would otherwise make its paper the longest section of the index and push it down in BM25.
const MAX_AUTHORS: usize = 25;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Paper {
    pub id: String,
    pub title: String,
    pub authors: Vec<String>,
    /// Authors beyond `authors` that the file leaves out.
    pub more_authors: usize,
    pub categories: Vec<String>,
    pub updated: String,
    pub comments: Option<String>,
    pub journal: Option<String>,
    pub doi: Option<String>,
    pub abstract_: String,
}

/// Collapses runs of whitespace (arXiv's XML wraps and indents titles and abstracts).
pub fn squash(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Normalizes what a user or a model passes as an id: `arXiv:2405.12345v2`, `2405.12345`,
/// `https://arxiv.org/abs/2405.12345`, `https://www.alphaxiv.org/abs/2405.12345`, `.../pdf/2405.12345v1.pdf`,
/// `cs/0601001`. Returns the bare id without version.
pub fn normalize_id(raw: &str) -> Option<String> {
    let mut s = raw.trim();
    for marker in ["/abs/", "/pdf/", "/html/", "/overview/"] {
        if let Some(i) = s.find(marker) {
            s = &s[i + marker.len()..];
            break;
        }
    }
    s = s.trim_start_matches("arXiv:").trim_start_matches("arxiv:");
    s = s.split(['?', '#']).next().unwrap_or(s);
    s = s.trim_end_matches('/').trim_end_matches(".pdf");
    // Drop a version suffix: 2405.12345v2 -> 2405.12345.
    if let Some(v) = s.rfind('v') {
        if v > 0 && s[v + 1..].chars().all(|c| c.is_ascii_digit()) && !s[v + 1..].is_empty() {
            s = &s[..v];
        }
    }
    let (archive, number) = match s.split_once('/') {
        Some((a, n)) => (Some(a), n),
        None => (None, s),
    };
    let valid = match archive {
        // Old style: archive/YYMMNNN.
        Some(a) => {
            !a.is_empty()
                && a.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
                && number.len() == 7
                && number.chars().all(|c| c.is_ascii_digit())
        }
        // New style: YYMM.NNNN or YYMM.NNNNN.
        None => {
            let (a, b) = number.split_once('.').unwrap_or(("", ""));
            a.len() == 4 && (4..=5).contains(&b.len()) && a.chars().chain(b.chars()).all(|c| c.is_ascii_digit())
        }
    };
    valid.then(|| s.to_string())
}

/// The year and month a paper was first submitted, from its id (both id styles start with YYMM).
pub fn id_month(id: &str) -> Option<(u16, u8)> {
    let digits = id.rsplit('/').next()?;
    let yy: u16 = digits.get(0..2)?.parse().ok()?;
    let mm: u8 = digits.get(2..4)?.parse().ok()?;
    if !(1..=12).contains(&mm) {
        return None;
    }
    // arXiv started in 1991: 91..99 are the 1990s.
    let year = if yy >= 91 { 1900 + yy } else { 2000 + yy };
    Some((year, mm))
}

/// Where a paper's file lives below the corpus folder.
pub fn rel_path(id: &str) -> PathBuf {
    let number = id.rsplit('/').next().unwrap_or(id);
    let month = number.get(0..4).unwrap_or("0000");
    PathBuf::from(month).join(format!("{}.md", id.replace('/', "_")))
}

/// The id a corpus file holds, from its name.
pub fn id_of_path(path: &Path) -> Option<String> {
    let stem = path.file_stem()?.to_str()?;
    Some(stem.replace('_', "/"))
}

impl Paper {
    pub fn to_markdown(&self) -> String {
        let mut out = String::with_capacity(self.abstract_.len() + 512);
        out.push_str("# ");
        out.push_str(self.title.trim_start_matches('#').trim());
        out.push_str("\n\n");
        if !self.authors.is_empty() {
            out.push_str("Authors: ");
            out.push_str(&self.authors.join(", "));
            if self.more_authors > 0 {
                out.push_str(&format!(" and {} more", self.more_authors));
            }
            out.push('\n');
        }
        out.push_str("Categories: ");
        out.push_str(&self.categories.join(" "));
        out.push('\n');
        if !self.updated.is_empty() {
            out.push_str(&format!("Updated: {}\n", self.updated));
        }
        for (key, value) in [("Comments", &self.comments), ("Journal", &self.journal), ("DOI", &self.doi)] {
            if let Some(v) = value {
                out.push_str(&format!("{key}: {v}\n"));
            }
        }
        out.push('\n');
        // One line: an abstract can never open a heading or a code fence of its own.
        out.push_str(self.abstract_.trim_start_matches(['#', '`', '~']).trim());
        out.push('\n');
        out
    }

    pub fn from_markdown(id: &str, text: &str) -> Paper {
        let mut p = Paper { id: id.to_string(), ..Paper::default() };
        let mut lines = text.lines();
        if let Some(first) = lines.next() {
            p.title = first.trim_start_matches('#').trim().to_string();
        }
        let mut in_meta = false;
        let mut body = Vec::new();
        for line in lines {
            if body.is_empty() && !in_meta && line.is_empty() {
                in_meta = true;
                continue;
            }
            if in_meta {
                if line.is_empty() {
                    in_meta = false;
                    body.push("");
                    continue;
                }
                let (key, value) = line.split_once(": ").unwrap_or(("", line));
                match key {
                    "Authors" => {
                        let (names, more) = match value.rsplit_once(" and ") {
                            Some((n, m)) if m.ends_with(" more") => {
                                (n, m.trim_end_matches(" more").parse().unwrap_or(0))
                            }
                            _ => (value, 0),
                        };
                        p.authors = names.split(", ").map(str::to_string).collect();
                        p.more_authors = more;
                    }
                    "Categories" => p.categories = value.split_whitespace().map(str::to_string).collect(),
                    "Updated" => p.updated = value.to_string(),
                    "Comments" => p.comments = Some(value.to_string()),
                    "Journal" => p.journal = Some(value.to_string()),
                    "DOI" => p.doi = Some(value.to_string()),
                    _ => {}
                }
            } else {
                body.push(line);
            }
        }
        p.abstract_ = body.join("\n").trim().to_string();
        p
    }

    /// Cuts the author list to what a file keeps.
    pub fn cap_authors(&mut self) {
        if self.authors.len() > MAX_AUTHORS {
            self.more_authors += self.authors.len() - MAX_AUTHORS;
            self.authors.truncate(MAX_AUTHORS);
        }
    }

    pub fn month(&self) -> Option<(u16, u8)> {
        id_month(&self.id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_normalized() {
        assert_eq!(normalize_id("arXiv:2405.12345v2").as_deref(), Some("2405.12345"));
        assert_eq!(normalize_id("https://www.alphaxiv.org/abs/2405.1234").as_deref(), Some("2405.1234"));
        assert_eq!(normalize_id("https://arxiv.org/pdf/2405.12345v1.pdf").as_deref(), Some("2405.12345"));
        assert_eq!(normalize_id("hep-th/9901001v3").as_deref(), Some("hep-th/9901001"));
        assert_eq!(normalize_id("cs/0601001").as_deref(), Some("cs/0601001"));
        assert_eq!(normalize_id("attention"), None);
        assert_eq!(normalize_id("2405.123"), None);
    }

    #[test]
    fn months_and_paths() {
        assert_eq!(id_month("2405.12345"), Some((2024, 5)));
        assert_eq!(id_month("hep-th/9901001"), Some((1999, 1)));
        assert_eq!(rel_path("cs/0601001"), PathBuf::from("0601/cs_0601001.md"));
        assert_eq!(id_of_path(&rel_path("cs/0601001")).as_deref(), Some("cs/0601001"));
        assert_eq!(id_of_path(&rel_path("2405.12345")).as_deref(), Some("2405.12345"));
    }

    #[test]
    fn markdown_round_trips() {
        let mut p = Paper {
            id: "2405.12345".into(),
            title: "LoRA: Low-Rank Adaptation".into(),
            authors: (0..30).map(|i| format!("Author {i}")).collect(),
            categories: vec!["cs.CL".into(), "cs.LG".into()],
            updated: "2024-06-01".into(),
            comments: Some("ICLR 2025".into()),
            journal: None,
            doi: Some("10.1/x".into()),
            abstract_: "We propose LoRA. It works.".into(),
            more_authors: 0,
        };
        p.cap_authors();
        let back = Paper::from_markdown("2405.12345", &p.to_markdown());
        assert_eq!(back, p);
        assert_eq!(back.more_authors, 5);
    }
}
