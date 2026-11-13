# fastxiv

A local arXiv search for LLM agents, in Rust. It harvests arXiv's metadata (titles, authors, abstracts) for the AI and
machine-learning categories, indexes it with [docsearch](https://github.com/Code000-211/microPi/tree/main/packages/docsearch)
(µPi's `project_search` core) and serves it to µPi, or any MCP client, as three tools: `search`, `paper` and `related`.

- **Retrieval without a model at search time.** BM25 over each paper, plus a graph of papers and the names they mention
  (models, methods, datasets: LoRA, GPT-4, MMLU, KV-cache …), walked with personalized PageRank. This is the retrieval of
  HippoRAG, with rule-based entities in place of an LLM. It is not Microsoft's GraphRAG: there is no LLM extraction and no
  community summaries, so indexing costs nothing and a search takes milliseconds.
- **One binary, no services.** The index is a file that is memory-mapped. The server is ready in about a second and keeps
  itself current while a daily harvest adds papers.

## Measured

On a 4-core cloud VM, release build, 10 queries each repeated 20 times over MCP (stdio), with the first 10 calls dropped:

| Corpus | Index build | Index file | Search, k=10, p50 / p90 | Server RSS |
|---|---:|---:|---:|---:|
| 14,822 papers (real: cs + stat, 3 weeks of OAI updates) | 1.5 s | 72 MB | 1.2–2 ms (cold process) | – |
| 295,340 papers (the real ones copied 20 times) | 29 s | 1.25 GB | 1.8 / 3.9 ms | 313 MB (108 MB private) |

Search times include reading and formatting the 10 papers that are returned. With filters (`since`, `categories`) the
p50 is 2.2 ms. Starting with a saved index takes about 1 s for 295k files, which is one scan of the corpus folder. The
copied corpus repeats every text 20 times, so its posting lists are longer than those of a real corpus of the same size:
treat those numbers as an estimate.

A harvest is limited by arXiv's server, not by fastxiv: about 9 s per page of 1,300 records. Three weeks of updates took
3.5 minutes. A first full harvest of `cs` and `stat` is in the order of an hour or two (not measured).

## Retrieval quality

`eval/known-items.tsv` holds 70 known-item queries for 42 well-known papers (Transformer, BERT, LoRA, DPO, FlashAttention
…). There are two kinds: `name` (how people refer to a paper) and `desc` (what it does, without its title's words).
`fastxiv eval` reports Hit@1/3/10, MRR, and the queries that miss the top 3.

| Corpus | Hit@1 | Hit@3 | Hit@10 | MRR |
|---|---:|---:|---:|---:|
| 14,822 recent papers plus the 42 targets, fresh index | 81% | 86% | 99% | 0.855 |
| The same, with the 42 targets in the overlay of changed files | 50% | 73% | 96% | 0.647 |

### Citation prior

`fastxiv citations` fetches citation counts from Semantic Scholar's batch API (500 papers per request, no key needed)
into `citations.tsv`. Search then re-ranks the top 50 by `text score / best score + w · log10(1 + citations per year)`.
The defaults are `w = 0.2`, re-ranking the top 200 text matches (`--prior` and `--pool` on `search`/`serve`/`eval`); set it with `--prior` on `search` and `serve`, and use 0 to turn it off. Results show
their citation count. Semantic Scholar merges a preprint with its published version, so an arXiv id gets both
versions' citations. On the local corpus, 14,713 of 14,864 papers had a count; fetching them took 2 minutes.

| Prior weight | Hit@1 | Hit@3 | Hit@10 | MRR |
|---:|---:|---:|---:|---:|
| 0 (off) | 81% | 86% | 99% | 0.855 |
| 0.05 | 84% | 93% | 99% | 0.890 |
| 0.1 | 91% | 99% | 99% | 0.945 |
| 0.2 – 0.5 | 96% | 99% | 99% | 0.969 |

On the server's full corpus (525,906 papers, 524,016 with a count, fetched in 59 minutes):

| Prior weight | Hit@1 | Hit@3 | Hit@10 | MRR |
|---:|---:|---:|---:|---:|
| 0 (off) | 49% | 61% | 73% | 0.564 |
| 0.05 | 64% | 70% | 83% | 0.692 |
| 0.1 | 71% | 77% | 83% | 0.751 |
| 0.2 | 77% | 81% | 83% | 0.793 |
| 0.3 | 79% | 83% | 83% | 0.805 |

With 50 candidates Hit@10 stays at 83%: in 12 of the 70 cases the target is not among the text matches the prior
ranks again. Ranking more of them helps, up to a point:

| Prior, pool | Hit@1 | Hit@3 | Hit@10 | MRR | Search |
|---|---:|---:|---:|---:|---:|
| 0.2, 50 | 77% | 81% | 83% | 0.793 | 4.8 ms |
| **0.2, 200 (default)** | **81%** | **89%** | **91%** | **0.854** | **6.4 ms** |
| 0.2, 500 | 81% | 89% | 91% | 0.854 | 8.7 ms |
| 0.3, 200 | 86% | 91% | 91% | 0.883 | 6.4 ms |

At 0.3 and above, older well-cited neighbours start to win (DPR for the RAG paper, DDIM for DDPM). The gap from 0.2
to 0.3 is 3–4 of 70 queries on a set that favours famous papers, so the default stays at 0.2. Search times are from a
fresh CLI process.

The known-item set favours famous papers, so a strong prior always looks good on it. Topic searches ("diffusion
language models", "KV cache compression") keep showing recent papers at 0.1. At 0.2 and above, the more-cited ones
move up. `fastxiv eval --prior 0,0.05,0.1,0.2` compares weights on your own index. The weekly
`fastxiv-citations.timer` keeps the counts current.

A small corpus is easier than the full one: measure on the server's 525k papers before trusting these numbers. Papers in
the overlay rank lower until the index is rebuilt. That is why `fastxiv index` always rebuilds, and why the server
folds an overlay into a rebuilt index after at most 10 minutes. The misses are mostly famous papers
outranked by recent ones that reuse their title words ("SDS-LoRA", "TLRS-DPO"). A citation-count signal is the
planned fix.

## Build

Needs Rust (stable) and git. The docsearch core comes from microPi, pinned to a commit in `Cargo.toml`.

```sh
cargo build --release      # -> target/release/fastxiv
cargo test
```

## Use

```sh
export FASTXIV_DATA=/var/lib/fastxiv     # default: ~/.local/share/fastxiv
fastxiv harvest                          # first run: everything in cs and stat; later runs: only what changed
fastxiv index                            # build the index now (the server would also do it on start)
fastxiv search "reduce KV cache memory for long context" -k 5 --since 2025
fastxiv paper 2106.09685                 # also arXiv:2106.09685v2 or an arxiv.org / alphaxiv.org URL
fastxiv related 2106.09685
fastxiv eval                             # Hit@1/3/10 and MRR on eval/known-items.tsv
fastxiv serve                            # MCP over stdio
fastxiv serve --http 127.0.0.1:8765      # MCP over Streamable HTTP at /mcp, health at /health
```

`harvest` keeps papers that have at least one of these categories: `cs.AI cs.CL cs.LG cs.IR cs.MA cs.NE stat.ML`. Change
this with `--categories cs.CL,cs.CV,...`, or keep everything in the sets with `--categories all`. The sets themselves are
chosen with `--sets cs,stat,eess`. A paper whose categories later move away from the kept ones is removed, and so is one
that arXiv deletes. Files whose contents did not change are not rewritten.

### Layout of the data folder

```
$FASTXIV_DATA/
  corpus/2106/2106.09685.md   one Markdown file per paper: "# Title", Authors/Categories/... lines, the abstract
  corpus/0601/cs_0601001.md   old-style ids, with "/" written as "_"
  index/<hash>.idx            the saved index (docsearch's format), mapped by every process that uses it
  state.json                  where the next incremental harvest starts
```

## Run it on your server

```sh
sudo useradd --system --home /var/lib/fastxiv --create-home fastxiv
sudo install -m 755 target/release/fastxiv /usr/local/bin/
sudo -u fastxiv FASTXIV_DATA=/var/lib/fastxiv fastxiv harvest     # the first full harvest; takes a while
sudo cp deploy/*.service deploy/*.timer /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now fastxiv.service fastxiv-harvest.timer fastxiv-citations.timer
sudo systemctl start --no-block fastxiv-citations.service   # first citation fetch, about an hour
curl -s 127.0.0.1:8765/health
```

The server listens on 127.0.0.1 only. The daily timer fetches what changed, and the server finds the new files within
`--refresh-secs` (5 minutes). Changes of up to 200 files go into a small overlay index at once. More changes than that
trigger a rebuild in the background, while searches keep using the old index.

To require a token, write `FASTXIV_TOKEN=<random string>` to `/etc/fastxiv.env` (mode 600) and restart the service.
Without a token, the server rejects browser requests from origins other than localhost. It speaks plain HTTP. To reach
it from another machine, use an SSH tunnel (`ssh -L 8765:127.0.0.1:8765 server`) or a VPN such as WireGuard or
Tailscale, rather than exposing the port to the internet.

## Connect µPi

Add the server to `~/.pi/agent/mcp.json`, or to `<project>/.pi/mcp.json` in a trusted project (see `deploy/mcp.json`).

Over HTTP, either on the server itself or through the SSH tunnel:

```json
{ "mcpServers": { "arxiv": { "url": "http://127.0.0.1:8765/mcp", "headers": { "Authorization": "Bearer ${FASTXIV_TOKEN}" } } } }
```

If no token is set, leave out `headers`. µPi then tries OAuth only when the server answers 401, and this server never
does.

When µPi runs on the server itself, it can also start fastxiv over stdio. The index file is shared, so this costs a
second of start-up per session and no extra memory:

```json
{ "mcpServers": { "arxiv": { "command": "fastxiv", "args": ["serve", "--refresh-secs", "0"], "env": { "FASTXIV_DATA": "/var/lib/fastxiv" } } } }
```

`/mcp` in µPi shows the server and its tools.

## Tools

| Tool | Arguments | Returns |
|---|---|---|
| `search` | `query`, `k` (10), `since`/`until` (`YYYY` or `YYYY-MM`), `categories`, `detail` (3) | ids, titles, months, categories, authors; abstracts of the top `detail` results |
| `paper` | `id` (id, versioned id or arxiv.org/alphaxiv.org URL) | all metadata, the abstract, links to arXiv, the PDF and alphaXiv |
| `related` | `id`, `k` (10) | papers on the same methods, models or topic (searched with the paper's title and abstract) |

## Limits and next steps

- **Descriptive queries.** BM25 plus the graph finds names and keywords well. On docsearch's own code evaluation, a
  query that describes something without naming it found the target at rank 1 only 15% of the time. Dense
  embeddings, fused with this ranking by reciprocal rank fusion, are the next step. A query embedding costs about 20 ms
  on a CPU, which is negligible next to an agent's turn.
- **Citations.** OpenAlex or Semantic Scholar citation edges in the graph, so that `related` follows citations and not
  only shared words and names.
- **Full text.** arXiv allows metadata harvesting over OAI, but not harvesting full texts that way (see `dataPolicy` in
  `?verb=Identify`). Full text has to come from arXiv's bulk data. docsearch already splits Markdown at its headings, so
  one section per paper section would fit as is.
- **Evaluation.** A query set with known target papers, measured as Hit@k (like `eval/` in docsearch), plus agent turns
  per research question.
