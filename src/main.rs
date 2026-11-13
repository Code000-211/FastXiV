//! fastxiv: a local arXiv search for LLM agents.

mod citations;
mod eval;
mod format;
mod harvest;
mod library;
mod mcp;
mod paper;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use library::{parse_month, Library, Query};

const USAGE: &str = "fastxiv: local arXiv search for LLM agents (MCP)

Usage: fastxiv [--data DIR] <command> [options]

Commands:
  harvest   Fetch arXiv metadata over OAI-PMH into DIR/corpus (incremental after the first run)
              --from YYYY-MM-DD  --until YYYY-MM-DD  --sets cs,stat
              --categories cs.AI,cs.CL,... | all   --delay-ms 2000  --base-url URL
  index     Build or update the search index and wait until it is saved
  search    Search from the command line
              -k N  --since YYYY[-MM]  --until YYYY[-MM]  --categories cs.CL,...  --detail N  QUERY...
  paper     Show one paper: ID
  related   Papers related to one: [-k N] ID
  citations Fetch citation counts from Semantic Scholar into DIR/citations.tsv (a ranking prior)
              --missing (only papers without a count yet)  --delay-ms 1100
              Set SEMANTIC_SCHOLAR_API_KEY to use a key (higher rate limit).
  eval      Measure Hit@1/3/10 and MRR on a known-item set: [-k N] [FILE] (default eval/known-items.tsv)
              --prior 0,0.1,0.2 and --pool 50,200 compare weights of the citation prior and how many
              text matches it ranks again
  serve     Run the MCP server: over stdio, or over HTTP with --http ADDR (e.g. 127.0.0.1:8765)
              --threads N (default 4)  --refresh-secs N (default 300; 0: never)
              Set FASTXIV_TOKEN to require `Authorization: Bearer <token>` over HTTP.

DIR defaults to $FASTXIV_DATA, else ~/.local/share/fastxiv.";

struct Args {
    rest: Vec<String>,
}

impl Args {
    /// Removes `--name VALUE` (or `--name=VALUE`) and returns VALUE.
    fn opt(&mut self, names: &[&str]) -> Result<Option<String>, String> {
        let mut i = 0;
        while i < self.rest.len() {
            let a = &self.rest[i];
            if a == "--" {
                break;
            }
            for n in names {
                if a == n {
                    if i + 1 >= self.rest.len() {
                        return Err(format!("{n} needs a value"));
                    }
                    let v = self.rest.remove(i + 1);
                    self.rest.remove(i);
                    return Ok(Some(v));
                }
                if let Some(v) = a.strip_prefix(&format!("{n}=")) {
                    let v = v.to_string();
                    self.rest.remove(i);
                    return Ok(Some(v));
                }
            }
            i += 1;
        }
        Ok(None)
    }

    fn num(&mut self, names: &[&str], default: usize) -> Result<usize, String> {
        match self.opt(names)? {
            None => Ok(default),
            Some(v) => v.parse().map_err(|_| format!("{} needs a number, not \"{v}\"", names[0])),
        }
    }

    fn list(&mut self, names: &[&str]) -> Result<Option<Vec<String>>, String> {
        Ok(self.opt(names)?.map(|v| v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()))
    }

    fn month(&mut self, names: &[&str]) -> Result<Option<(u16, u8)>, String> {
        match self.opt(names)? {
            None => Ok(None),
            Some(v) => parse_month(&v).map(Some).ok_or_else(|| format!("{} must be YYYY or YYYY-MM", names[0])),
        }
    }

    fn words(&mut self) -> String {
        self.rest.retain(|a| a != "--");
        self.rest.join(" ")
    }
}

fn default_data() -> PathBuf {
    if let Some(d) = std::env::var_os("FASTXIV_DATA") {
        return PathBuf::from(d);
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    home.join(".local/share/fastxiv")
}

fn wait_for_index(lib: &Library) -> Result<Arc<docsearch_core::View>, String> {
    let t0 = Instant::now();
    let view = lib.view(Duration::from_secs(24 * 3600)).ok_or("no index")?;
    // The first index may still be saving in the background: let it finish before exiting.
    while lib.building() {
        std::thread::sleep(Duration::from_millis(50));
    }
    let view = lib.view(Duration::ZERO).unwrap_or(view);
    if t0.elapsed() > Duration::from_secs(2) {
        eprintln!("index ready after {:.1} s", t0.elapsed().as_secs_f64());
    }
    Ok(view)
}

fn run() -> Result<(), String> {
    let mut args = Args { rest: std::env::args().skip(1).collect() };
    if args.rest.is_empty() || args.rest.iter().any(|a| a == "-h" || a == "--help") {
        println!("{USAGE}");
        return Ok(());
    }
    let data = args.opt(&["--data"])?.map(PathBuf::from).unwrap_or_else(default_data);
    let command = args.rest.remove(0);
    match command.as_str() {
        "harvest" => {
            let categories = match args.list(&["--categories"])? {
                Some(c) if c.iter().any(|x| x == "all") => None,
                Some(c) => Some(c),
                None => Some(harvest::DEFAULT_CATEGORIES.iter().map(|s| s.to_string()).collect()),
            };
            let opts = harvest::Options {
                base_url: args.opt(&["--base-url"])?.unwrap_or_else(|| harvest::DEFAULT_BASE_URL.to_string()),
                sets: args
                    .list(&["--sets"])?
                    .unwrap_or_else(|| harvest::DEFAULT_SETS.iter().map(|s| s.to_string()).collect()),
                from: args.opt(&["--from"])?,
                until: args.opt(&["--until"])?,
                categories,
                delay: Duration::from_millis(args.num(&["--delay-ms"], 2000)? as u64),
            };
            let t0 = Instant::now();
            let stats = harvest::run(&data.join("corpus"), &data.join("state.json"), &opts)?;
            eprintln!("harvest done in {:.0} s: {stats:?}", t0.elapsed().as_secs_f64());
        }
        "index" => {
            let lib = Library::open(&data);
            wait_for_index(&lib)?;
            // Fold changed papers into a rebuilt, saved index (an overlay would rank them lower).
            lib.rebuild();
            std::thread::sleep(Duration::from_millis(100));
            let view = wait_for_index(&lib)?;
            println!(
                "{} papers, {} sections, {} terms, {} entities (built in {} ms)",
                view.base.file_count(),
                view.base.section_count(),
                view.base.term_count(),
                view.base.entity_count(),
                view.base.build_ms
            );
        }
        "search" => {
            let k = args.num(&["-k", "--k"], 10)?;
            let detail = args.num(&["--detail"], 3)?;
            let since = args.month(&["--since"])?;
            let until = args.month(&["--until"])?;
            let categories = args.list(&["--categories", "--cat"])?.unwrap_or_default();
            let prior =
                args.opt(&["--prior"])?.map(|v| v.parse::<f32>()).transpose().map_err(|_| "--prior needs a number")?;
            let query = args.words();
            if query.is_empty() {
                return Err("search needs a query".into());
            }
            let lib = Library::open(&data);
            let view = wait_for_index(&lib)?;
            let q = Query {
                text: &query,
                k: k.max(1),
                since,
                until,
                categories: &categories,
                exclude: None,
                prior,
                pool: None,
            };
            print!("{}", format::answer(&lib.search(&view, &q), &query, detail));
        }
        "paper" => {
            let id = args.words();
            let lib = Library::open(&data);
            let p = lib.paper(&id).ok_or_else(|| format!("no paper \"{id}\" in {}", data.display()))?;
            print!("{}", format::paper(&p));
        }
        "related" => {
            let k = args.num(&["-k", "--k"], 10)?;
            let id = args.words();
            let lib = Library::open(&data);
            let p = lib.paper(&id).ok_or_else(|| format!("no paper \"{id}\" in {}", data.display()))?;
            let view = wait_for_index(&lib)?;
            print!("{}", format::answer(&lib.related(&view, &p, k.max(1)), &format!("related to {}", p.id), 3));
        }
        "eval" => {
            let k = args.num(&["-k", "--k"], 10)?;
            let priors: Vec<Option<f32>> = match args.list(&["--prior"])? {
                None => vec![None],
                Some(ws) => ws
                    .iter()
                    .map(|w| w.parse::<f32>().map(Some).map_err(|_| format!("--prior: \"{w}\" is not a number")))
                    .collect::<Result<_, _>>()?,
            };
            let pools: Vec<Option<usize>> = match args.list(&["--pool"])? {
                None => vec![None],
                Some(ns) => ns
                    .iter()
                    .map(|n| n.parse::<usize>().map(Some).map_err(|_| format!("--pool: \"{n}\" is not a number")))
                    .collect::<Result<_, _>>()?,
            };
            let file = args.words();
            let file = if file.is_empty() { "eval/known-items.tsv".to_string() } else { file };
            let text = std::fs::read_to_string(&file).map_err(|e| format!("{file}: {e}"))?;
            let cases = eval::parse(&text)?;
            let lib = Library::open(&data);
            let view = wait_for_index(&lib)?;
            eprintln!("{} citation counts loaded", lib.citations().len());
            for prior in &priors {
                for pool in &pools {
                    let w = prior.unwrap_or(lib.default_prior);
                    let n = pool.unwrap_or(lib.default_pool);
                    println!("=== citation prior {w}, pool {n}");
                    print!("{}", eval::run(&lib, &view, &cases, k.max(1), *prior, *pool));
                }
            }
        }
        "citations" => {
            let missing = args.rest.iter().any(|a| a == "--missing");
            args.rest.retain(|a| a != "--missing");
            let delay = Duration::from_millis(args.num(&["--delay-ms"], 1100)? as u64);
            let key = std::env::var("SEMANTIC_SCHOLAR_API_KEY").ok().filter(|k| !k.is_empty());
            let t0 = Instant::now();
            let (known, asked) = citations::fetch(&data, missing, delay, key.as_deref())?;
            eprintln!("citations done in {:.0} s: {known} of {asked} papers known", t0.elapsed().as_secs_f64());
        }
        "serve" => {
            let http = args.opt(&["--http"])?;
            let threads = args.num(&["--threads"], 4)?;
            let refresh = args.num(&["--refresh-secs"], 300)?;
            let prior =
                args.opt(&["--prior"])?.map(|v| v.parse::<f32>()).transpose().map_err(|_| "--prior needs a number")?;
            let token = std::env::var("FASTXIV_TOKEN").ok().filter(|t| !t.is_empty());
            let mut lib = Library::open(&data);
            if let Some(w) = prior {
                lib.default_prior = w;
            }
            let lib = Arc::new(lib);
            if refresh > 0 {
                // Picks up what a harvest (a cron job) wrote, without a restart.
                let lib = Arc::clone(&lib);
                std::thread::spawn(move || loop {
                    std::thread::sleep(Duration::from_secs(refresh as u64));
                    lib.refresh();
                    lib.reload_citations();
                });
            }
            match http {
                Some(addr) => mcp::serve_http(lib, &addr, token, threads)?,
                None => mcp::serve_stdio(&lib),
            }
        }
        other => return Err(format!("unknown command \"{other}\"; see fastxiv --help")),
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("fastxiv: {e}");
        std::process::exit(1);
    }
}
