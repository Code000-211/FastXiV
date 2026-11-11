//! Harvesting arXiv's metadata over OAI-PMH (`metadataPrefix=arXiv`) into the corpus folder.
//!
//! arXiv permits metadata harvesting through OAI; full texts are not to be harvested this way
//! (see the `dataPolicy` in `?verb=Identify`). A first run fetches everything in the chosen sets;
//! later runs fetch from the date the previous one started (kept in `state.json`), so a daily
//! cron job picks up new and changed papers. Files whose contents did not change are not rewritten,
//! so the search index sees only real changes.

use std::io::Read;
use std::path::Path;
use std::time::Duration;

use quick_xml::events::Event;
use quick_xml::Reader;

use crate::paper::{self, Paper};

pub const DEFAULT_BASE_URL: &str = "https://oaipmh.arxiv.org/oai";
pub const DEFAULT_SETS: &[&str] = &["cs", "stat"];
/// The categories where research on LLMs and machine learning is published.
pub const DEFAULT_CATEGORIES: &[&str] = &["cs.AI", "cs.CL", "cs.LG", "cs.IR", "cs.MA", "cs.NE", "stat.ML"];

pub struct Options {
    pub base_url: String,
    /// OAI sets: an archive (`cs`, `stat`, `math`, `physics:hep-th`, ...).
    pub sets: Vec<String>,
    /// YYYY-MM-DD; default: where the last harvest started, or everything.
    pub from: Option<String>,
    pub until: Option<String>,
    /// Keep papers with at least one of these categories; `None` keeps all of the sets.
    pub categories: Option<Vec<String>>,
    /// Pause between requests.
    pub delay: Duration,
}

#[derive(Default, Debug)]
pub struct Stats {
    pub pages: usize,
    pub records: usize,
    pub written: usize,
    pub unchanged: usize,
    pub removed: usize,
    pub skipped: usize,
}

pub enum Record {
    Deleted(String),
    Paper(Paper),
}

#[derive(Default)]
pub struct Page {
    pub records: Vec<Record>,
    pub token: Option<String>,
    pub response_date: Option<String>,
    /// OAI error code and message.
    pub error: Option<(String, String)>,
}

/// Parses one ListRecords response.
pub fn parse_page(xml: &str) -> Result<Page, String> {
    let mut reader = Reader::from_str(xml);
    let mut page = Page::default();
    let mut stack: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut cur = Paper::default();
    let mut deleted = false;
    let mut identifier = String::new();
    let (mut keyname, mut forenames, mut suffix) = (String::new(), String::new(), String::new());
    let mut error_code = String::new();

    loop {
        match reader.read_event().map_err(|e| format!("XML error at {}: {e}", reader.buffer_position()))? {
            Event::Start(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                match name.as_str() {
                    "record" => {
                        cur = Paper::default();
                        deleted = false;
                        identifier.clear();
                    }
                    "header" => {
                        deleted = e
                            .attributes()
                            .flatten()
                            .any(|a| a.key.local_name().as_ref() == b"status" && a.value.as_ref() == b"deleted");
                    }
                    "author" => {
                        keyname.clear();
                        forenames.clear();
                        suffix.clear();
                    }
                    "error" => {
                        error_code = e
                            .attributes()
                            .flatten()
                            .find(|a| a.key.local_name().as_ref() == b"code")
                            .map(|a| String::from_utf8_lossy(&a.value).into_owned())
                            .unwrap_or_default();
                    }
                    _ => {}
                }
                stack.push(name);
                text.clear();
            }
            Event::Empty(_) => {}
            Event::Text(t) => text.push_str(&t.unescape().map_err(|e| e.to_string())?),
            Event::CData(t) => text.push_str(&String::from_utf8_lossy(&t)),
            Event::End(_) => {
                let name = stack.pop().unwrap_or_default();
                let parent = stack.last().map(String::as_str).unwrap_or("");
                let value = paper::squash(&text);
                match (parent, name.as_str()) {
                    (_, "responseDate") => page.response_date = Some(value),
                    (_, "resumptionToken") if !value.is_empty() => page.token = Some(value),
                    (_, "error") => page.error = Some((error_code.clone(), value)),
                    ("header", "identifier") => identifier = value,
                    ("arXiv", "id") => cur.id = value,
                    ("arXiv", "title") => cur.title = value,
                    ("arXiv", "abstract") => cur.abstract_ = value,
                    ("arXiv", "categories") => cur.categories = value.split_whitespace().map(str::to_string).collect(),
                    ("arXiv", "updated") => cur.updated = value,
                    ("arXiv", "created") if cur.updated.is_empty() => cur.updated = value,
                    ("arXiv", "comments") if !value.is_empty() => cur.comments = Some(value),
                    ("arXiv", "journal-ref") if !value.is_empty() => cur.journal = Some(value),
                    ("arXiv", "doi") if !value.is_empty() => cur.doi = Some(value),
                    ("author", "keyname") => keyname = value,
                    ("author", "forenames") => forenames = value,
                    ("author", "suffix") => suffix = value,
                    ("authors", "author") => {
                        let full = [forenames.as_str(), keyname.as_str(), suffix.as_str()]
                            .iter()
                            .filter(|s| !s.is_empty())
                            .copied()
                            .collect::<Vec<_>>()
                            .join(" ");
                        if !full.is_empty() {
                            cur.authors.push(full);
                        }
                    }
                    (_, "record") => {
                        let id = identifier.rsplit("arXiv.org:").next().unwrap_or("").to_string();
                        if deleted {
                            if !id.is_empty() {
                                page.records.push(Record::Deleted(id));
                            }
                        } else {
                            let mut p = std::mem::take(&mut cur);
                            if p.id.is_empty() {
                                p.id = id;
                            }
                            if !p.id.is_empty() {
                                page.records.push(Record::Paper(p));
                            }
                        }
                    }
                    _ => {}
                }
                text.clear();
            }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(page)
}

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .try_proxy_from_env(true)
        .timeout_connect(Duration::from_secs(30))
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("fastxiv/", env!("CARGO_PKG_VERSION")))
        .build()
}

/// GET with retries: OAI servers answer 503 with `Retry-After` to pace harvesters.
fn fetch(agent: &ureq::Agent, url: &str) -> Result<String, String> {
    let mut backoff = Duration::from_secs(5);
    let mut last = String::new();
    for _ in 0..8 {
        match agent.get(url).call() {
            Ok(resp) => {
                let mut body = String::new();
                match resp.into_reader().read_to_string(&mut body) {
                    Ok(_) => return Ok(body),
                    Err(e) => last = e.to_string(),
                }
            }
            Err(ureq::Error::Status(code, resp)) if code == 503 || code == 429 || code >= 500 => {
                let wait =
                    resp.header("Retry-After").and_then(|s| s.trim().parse::<u64>().ok()).map(Duration::from_secs);
                last = format!("HTTP {code}");
                std::thread::sleep(wait.unwrap_or(backoff).min(Duration::from_secs(600)));
                backoff = (backoff * 2).min(Duration::from_secs(300));
                continue;
            }
            Err(ureq::Error::Status(code, _)) => return Err(format!("HTTP {code} for {url}")),
            Err(e) => last = e.to_string(),
        }
        std::thread::sleep(backoff);
        backoff = (backoff * 2).min(Duration::from_secs(300));
    }
    Err(format!("giving up on {url}: {last}"))
}

enum Outcome {
    Written,
    Unchanged,
    Removed,
    Skipped,
}

fn write_atomic(path: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, contents)?;
    std::fs::rename(&tmp, path)
}

fn remove(path: &Path) -> Outcome {
    if std::fs::remove_file(path).is_ok() {
        Outcome::Removed
    } else {
        Outcome::Skipped
    }
}

fn store(corpus: &Path, record: Record, categories: Option<&[String]>) -> std::io::Result<Outcome> {
    match record {
        Record::Deleted(id) => Ok(remove(&corpus.join(paper::rel_path(&id)))),
        Record::Paper(mut p) => {
            let Some(id) = paper::normalize_id(&p.id) else { return Ok(Outcome::Skipped) };
            p.id = id;
            let path = corpus.join(paper::rel_path(&p.id));
            let keep = categories.is_none_or(|want| p.categories.iter().any(|c| want.contains(c)));
            if !keep {
                // Its categories may have changed away from the ones kept.
                return Ok(remove(&path));
            }
            p.cap_authors();
            let md = p.to_markdown();
            if std::fs::read_to_string(&path).is_ok_and(|old| old == md) {
                return Ok(Outcome::Unchanged);
            }
            write_atomic(&path, &md)?;
            Ok(Outcome::Written)
        }
    }
}

pub fn last_harvest(state_file: &Path) -> Option<String> {
    let s = std::fs::read_to_string(state_file).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("last_harvest")?.as_str().map(str::to_string)
}

pub fn run(corpus: &Path, state_file: &Path, opts: &Options) -> Result<Stats, String> {
    let agent = agent();
    let mut stats = Stats::default();
    let from = opts.from.clone().or_else(|| last_harvest(state_file));
    let mut started: Option<String> = None;
    std::fs::create_dir_all(corpus).map_err(|e| format!("{}: {e}", corpus.display()))?;

    for set in &opts.sets {
        let mut url = format!("{}?verb=ListRecords&metadataPrefix=arXiv&set={}", opts.base_url, percent_encode(set));
        if let Some(f) = &from {
            url.push_str(&format!("&from={}", percent_encode(f)));
        }
        if let Some(u) = &opts.until {
            url.push_str(&format!("&until={}", percent_encode(u)));
        }
        let mut set_records = 0usize;
        loop {
            let xml = fetch(&agent, &url)?;
            let page = parse_page(&xml)?;
            stats.pages += 1;
            if started.is_none() {
                // The server's date, not ours: the next run starts where this one did.
                started = page.response_date.as_ref().and_then(|d| d.get(0..10)).map(str::to_string);
            }
            if let Some((code, msg)) = &page.error {
                if code == "noRecordsMatch" {
                    break;
                }
                return Err(format!("OAI error {code}: {msg}"));
            }
            for record in page.records {
                stats.records += 1;
                set_records += 1;
                match store(corpus, record, opts.categories.as_deref()).map_err(|e| e.to_string())? {
                    Outcome::Written => stats.written += 1,
                    Outcome::Unchanged => stats.unchanged += 1,
                    Outcome::Removed => stats.removed += 1,
                    Outcome::Skipped => stats.skipped += 1,
                }
            }
            eprintln!(
                "{set}: {set_records} records ({} written, {} unchanged, {} removed, {} skipped)",
                stats.written, stats.unchanged, stats.removed, stats.skipped
            );
            let Some(token) = page.token else { break };
            url = format!("{}?verb=ListRecords&resumptionToken={}", opts.base_url, percent_encode(&token));
            std::thread::sleep(opts.delay);
        }
    }
    // A window with an end (a backfill) leaves the incremental position alone.
    if let (Some(date), None) = (started, &opts.until) {
        let state = serde_json::json!({ "last_harvest": date });
        write_atomic(state_file, &format!("{state:#}\n")).map_err(|e| e.to_string())?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<OAI-PMH xmlns="http://www.openarchives.org/OAI/2.0/">
  <responseDate>2026-11-11T12:49:48Z</responseDate>
  <ListRecords>
    <record>
      <header><identifier>oai:arXiv.org:2106.09685</identifier><datestamp>2026-11-05</datestamp></header>
      <metadata>
        <arXiv xmlns="http://arxiv.org/OAI/arXiv/">
          <id>2106.09685</id>
          <created>2021-06-17</created>
          <updated>2021-10-16</updated>
          <authors>
            <author><keyname>Hu</keyname><forenames>Edward J.</forenames></author>
            <author><keyname>Shen</keyname><forenames>Yelong</forenames></author>
          </authors>
          <title>LoRA: Low-Rank Adaptation of
            Large Language Models</title>
          <categories>cs.CL cs.AI cs.LG</categories>
          <comments>Draft V2 &amp; more</comments>
          <abstract>  An important paradigm of natural language processing consists of
  large-scale pre-training &lt;and&gt; adaptation.</abstract>
        </arXiv>
      </metadata>
    </record>
    <record>
      <header status="deleted"><identifier>oai:arXiv.org:cs/0601001</identifier></header>
    </record>
    <resumptionToken expirationDate="2026-11-12T00:00:00Z">verb%3DListRecords%26skip%3D1300</resumptionToken>
  </ListRecords>
</OAI-PMH>"#;

    #[test]
    fn parses_records_tokens_and_deletions() {
        let page = parse_page(SAMPLE).unwrap();
        assert_eq!(page.response_date.as_deref(), Some("2026-11-11T12:49:48Z"));
        assert_eq!(page.token.as_deref(), Some("verb%3DListRecords%26skip%3D1300"));
        assert_eq!(page.records.len(), 2);
        let Record::Paper(p) = &page.records[0] else { panic!() };
        assert_eq!(p.id, "2106.09685");
        assert_eq!(p.title, "LoRA: Low-Rank Adaptation of Large Language Models");
        assert_eq!(p.authors, ["Edward J. Hu", "Yelong Shen"]);
        assert_eq!(p.categories, ["cs.CL", "cs.AI", "cs.LG"]);
        assert_eq!(p.updated, "2021-10-16");
        assert_eq!(p.comments.as_deref(), Some("Draft V2 & more"));
        assert!(p.abstract_.starts_with("An important paradigm") && p.abstract_.contains("<and>"));
        assert!(matches!(&page.records[1], Record::Deleted(id) if id == "cs/0601001"));
    }

    #[test]
    fn oai_errors_are_reported() {
        let xml = r#"<OAI-PMH><responseDate>2026-11-11T00:00:00Z</responseDate><error code="noRecordsMatch">none</error></OAI-PMH>"#;
        let page = parse_page(xml).unwrap();
        assert_eq!(page.error, Some(("noRecordsMatch".to_string(), "none".to_string())));
    }
}
