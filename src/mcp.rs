//! A Model Context Protocol server with three tools (`search`, `paper`, `related`), over stdio
//! (newline-delimited JSON-RPC) or Streamable HTTP (POST /mcp answered with `application/json`,
//! no server-sent events: every call returns one result).

use std::io::{BufRead, Read, Write};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};

use crate::format;
use crate::library::{parse_month, Library, Query};

const SUPPORTED_VERSIONS: &[&str] = &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];
const FALLBACK_VERSION: &str = "2025-06-18";
/// How long a call waits for the first index of a fresh library.
const WAIT_FOR_INDEX: Duration = Duration::from_secs(20);
const MAX_BODY: u64 = 1 << 20;

const INSTRUCTIONS: &str = "Local arXiv search over AI, machine learning and LLM papers (titles, authors, abstracts). \
`search` takes about a millisecond, so run several focused searches rather than one broad one, and name the models, \
methods and datasets you know (they link papers in the index's graph). Use `related` to widen from a paper you found, \
`paper` for its full abstract and links. Cite papers by their arXiv id.";

fn tools() -> Value {
    json!([
        {
            "name": "search",
            "title": "Search arXiv papers",
            "description": "Search arXiv papers on AI, machine learning and LLMs in a local index. Ranks titles and abstracts \
    by BM25 together with a graph of the names papers mention (models, methods, datasets such as LoRA, GPT-4, MMLU), so \
    include such names when you know them; plain descriptions work too. Returns arXiv ids, titles, months, categories, \
    authors, and the abstracts of the top results. Takes ~1 ms: prefer several focused queries over one long one.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "Keywords, names or a short description." },
                    "k": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Number of results (default 10)." },
                    "since": { "type": "string", "description": "Only papers submitted in or after this month: YYYY or YYYY-MM." },
                    "until": { "type": "string", "description": "Only papers submitted in or before this month: YYYY or YYYY-MM." },
                    "categories": { "type": "array", "items": { "type": "string" }, "description": "Only papers in one of these arXiv categories, e.g. [\"cs.CL\"]." },
                    "detail": { "type": "integer", "minimum": 0, "maximum": 50, "description": "How many of the top results include their abstract (default 3)." }
                },
                "required": ["query"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "paper",
            "title": "Read an arXiv paper's abstract",
            "description": "Full metadata and abstract of one arXiv paper: authors, categories, comments (often the venue), \
    journal reference, DOI, and links to arXiv, the PDF and alphaXiv. Takes an id (2106.09685, arXiv:2106.09685v2) or an \
    arxiv.org / alphaxiv.org URL.",
            "inputSchema": {
                "type": "object",
                "properties": { "id": { "type": "string", "description": "arXiv id or URL." } },
                "required": ["id"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        },
        {
            "name": "related",
            "title": "Find related arXiv papers",
            "description": "Papers related to one arXiv paper (same methods, models, datasets or topic), found from its title \
    and abstract. Use it to widen a literature search from a paper you already have.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": { "type": "string", "description": "arXiv id or URL." },
                    "k": { "type": "integer", "minimum": 1, "maximum": 50, "description": "Number of results (default 10)." }
                },
                "required": ["id"]
            },
            "annotations": { "readOnlyHint": true, "openWorldHint": false }
        }
    ])
}

fn text_result(text: String, is_error: bool) -> Value {
    json!({ "content": [{ "type": "text", "text": text }], "isError": is_error })
}

fn arg_k(args: &Value, key: &str, default: usize, max: usize) -> usize {
    args.get(key).and_then(Value::as_u64).map_or(default, |v| (v as usize).clamp(0, max))
}

fn arg_month(args: &Value, key: &str) -> Result<Option<(u16, u8)>, String> {
    match args.get(key).and_then(Value::as_str).filter(|s| !s.trim().is_empty()) {
        None => Ok(None),
        Some(s) => parse_month(s).map(Some).ok_or_else(|| format!("`{key}` must be YYYY or YYYY-MM, not \"{s}\"")),
    }
}

fn arg_list(args: &Value, key: &str) -> Vec<String> {
    match args.get(key) {
        Some(Value::Array(items)) => items.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).collect(),
        Some(Value::String(s)) => s.split([',', ' ']).filter(|c| !c.is_empty()).map(str::to_string).collect(),
        _ => Vec::new(),
    }
}

fn call_tool(lib: &Library, name: &str, args: &Value) -> Result<String, String> {
    let view = lib.view(WAIT_FOR_INDEX).ok_or("The index is still being built; try again in a minute.")?;
    match name {
        "search" => {
            let query = args.get("query").and_then(Value::as_str).unwrap_or("").trim();
            if query.is_empty() {
                return Err("`query` is required".into());
            }
            let categories = arg_list(args, "categories");
            let q = Query {
                text: query,
                k: arg_k(args, "k", 10, 50).max(1),
                since: arg_month(args, "since")?,
                until: arg_month(args, "until")?,
                categories: &categories,
                exclude: None,
                prior: None,
                pool: None,
            };
            Ok(format::answer(&lib.search(&view, &q), query, arg_k(args, "detail", 3, 50)))
        }
        "paper" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or("");
            lib.paper(id).map(|p| format::paper(&p)).ok_or_else(|| format!("No paper \"{id}\" in the index."))
        }
        "related" => {
            let id = args.get("id").and_then(Value::as_str).unwrap_or("");
            let p = lib.paper(id).ok_or_else(|| format!("No paper \"{id}\" in the index."))?;
            let a = lib.related(&view, &p, arg_k(args, "k", 10, 50).max(1));
            Ok(format::answer(&a, &format!("related to {}", p.id), 3))
        }
        _ => Err(format!("Unknown tool \"{name}\"")),
    }
}

fn error(id: Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// Answers one JSON-RPC message; `None` for notifications.
fn handle_one(lib: &Library, msg: &Value) -> Option<Value> {
    let method = msg.get("method").and_then(Value::as_str);
    let id = msg.get("id").cloned();
    let Some(method) = method else {
        // A response from the client (we send no requests) or garbage: nothing to answer.
        return id.map(|id| error(id, -32600, "Invalid request"));
    };
    let id = id?; // notifications get no answer
    let params = msg.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "initialize" => {
            let asked = params.get("protocolVersion").and_then(Value::as_str).unwrap_or(FALLBACK_VERSION);
            let version = if SUPPORTED_VERSIONS.contains(&asked) { asked } else { FALLBACK_VERSION };
            json!({
                "protocolVersion": version,
                "capabilities": { "tools": { "listChanged": false } },
                "serverInfo": { "name": "fastxiv", "version": env!("CARGO_PKG_VERSION") },
                "instructions": INSTRUCTIONS
            })
        }
        "ping" => json!({}),
        "tools/list" => json!({ "tools": tools() }),
        "tools/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
            match call_tool(lib, name, &args) {
                Ok(text) => text_result(text, false),
                Err(e) => text_result(e, true),
            }
        }
        "resources/list" => json!({ "resources": [] }),
        "prompts/list" => json!({ "prompts": [] }),
        _ => return Some(error(id, -32601, &format!("Method not found: {method}"))),
    };
    Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }))
}

/// Answers a message or a batch.
pub fn handle(lib: &Library, msg: &Value) -> Option<Value> {
    match msg {
        Value::Array(batch) => {
            let out: Vec<Value> = batch.iter().filter_map(|m| handle_one(lib, m)).collect();
            (!out.is_empty()).then_some(Value::Array(out))
        }
        m => handle_one(lib, m),
    }
}

pub fn serve_stdio(lib: &Library) {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        let reply = match serde_json::from_str::<Value>(&line) {
            Ok(msg) => handle(lib, &msg),
            Err(_) => Some(error(Value::Null, -32700, "Parse error")),
        };
        if let Some(r) = reply {
            if writeln!(stdout, "{r}").and_then(|_| stdout.flush()).is_err() {
                break;
            }
        }
    }
}

fn header<'a>(req: &'a tiny_http::Request, name: &'static str) -> Option<&'a str> {
    req.headers().iter().find(|h| h.field.equiv(name)).map(|h| h.value.as_str())
}

fn is_local_origin(origin: &str) -> bool {
    let host = origin.split("://").nth(1).unwrap_or(origin);
    let host = host.split('/').next().unwrap_or(host);
    let host = if host.starts_with('[') {
        host.split(']').next().map(|h| &h[1..]).unwrap_or(host)
    } else {
        host.split(':').next().unwrap_or(host)
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

fn respond(req: tiny_http::Request, status: u16, body: String, content_type: &str) {
    let ct = tiny_http::Header::from_bytes(&b"Content-Type"[..], content_type.as_bytes()).expect("valid header");
    let _ = req.respond(tiny_http::Response::from_string(body).with_status_code(status).with_header(ct));
}

fn handle_http(lib: &Library, mut req: tiny_http::Request, token: Option<&str>) {
    let path = req.url().split('?').next().unwrap_or("").to_string();
    if path == "/health" {
        let ready = lib.view(Duration::ZERO);
        let body = json!({
            "ready": ready.is_some(),
            "building": lib.building(),
            "papers": ready.as_ref().map_or(0, |v| v.base.file_count()),
        });
        return respond(req, 200, body.to_string(), "application/json");
    }
    if path != "/mcp" && path != "/" {
        return respond(req, 404, "not found".into(), "text/plain");
    }
    if let Some(t) = token {
        let ok =
            header(&req, "Authorization").is_some_and(|h| h.strip_prefix("Bearer ").is_some_and(|v| v.trim() == t));
        if !ok {
            return respond(req, 401, "unauthorized".into(), "text/plain");
        }
    } else if header(&req, "Origin").is_some_and(|o| !is_local_origin(o)) {
        // Without a token only local pages may call (DNS rebinding protection, as MCP asks).
        return respond(req, 403, "forbidden origin".into(), "text/plain");
    }
    if *req.method() != tiny_http::Method::Post {
        // No server-to-client stream: clients see 405 and go on without one.
        return respond(req, 405, "method not allowed".into(), "text/plain");
    }
    let mut body = String::new();
    if req.as_reader().take(MAX_BODY).read_to_string(&mut body).is_err() {
        return respond(req, 400, "bad body".into(), "text/plain");
    }
    let reply = match serde_json::from_str::<Value>(&body) {
        Ok(msg) => handle(lib, &msg),
        Err(_) => Some(error(Value::Null, -32700, "Parse error")),
    };
    match reply {
        Some(r) => respond(req, 200, r.to_string(), "application/json"),
        None => {
            let _ = req.respond(tiny_http::Response::empty(202));
        }
    }
}

pub fn serve_http(lib: Arc<Library>, addr: &str, token: Option<String>, threads: usize) -> Result<(), String> {
    let server = Arc::new(tiny_http::Server::http(addr).map_err(|e| format!("cannot listen on {addr}: {e}"))?);
    eprintln!("fastxiv: MCP on http://{addr}/mcp{}", if token.is_some() { " (bearer token required)" } else { "" });
    let token = token.map(Arc::<str>::from);
    let workers: Vec<_> = (0..threads.max(1))
        .map(|_| {
            let (server, lib, token) = (Arc::clone(&server), Arc::clone(&lib), token.clone());
            std::thread::spawn(move || {
                for req in server.incoming_requests() {
                    handle_http(&lib, req, token.as_deref());
                }
            })
        })
        .collect();
    for w in workers {
        let _ = w.join();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn origins() {
        assert!(is_local_origin("http://localhost:3000"));
        assert!(is_local_origin("http://127.0.0.1"));
        assert!(is_local_origin("http://[::1]:8080"));
        assert!(!is_local_origin("https://evil.example"));
        assert!(!is_local_origin("http://localhost.evil.example"));
    }
}
