# grafeno-server

The `grafeno` binary serves and manages a Grafeno database (installers:
[docs/install.md](../../docs/install.md)):

- `grafeno serve`: a JSON HTTP API, for applications such as Grafito, and
  Grafeno Studio, the web interface, at `/studio/`.
- `grafeno config init | path | token`: the settings file of an installed
  server (`grafeno.toml`).
- `grafeno service install | uninstall`: the Windows service.
- `grafeno mcp`: an MCP server over stdio, for agents (Claude Code, Claude
  Desktop and any other MCP client).
- `grafeno shell`: an interactive console for GQ queries and
  administration, on a file or on a running server (like `psql`).
- `grafeno namespaces | stats | check | rebuild | export | import`:
  administration commands on a file (like `pg_dump`).
- `grafeno keys list | create | revoke`: API keys, on a file or through a
  running server (see *Access* below).
- `grafeno backup OUT`: a consistent copy of the whole database, from a
  file or (with `--config`) from a running server, which keeps serving.
  `grafeno check` verifies every page of the file against its checksum
  before checking the indexes: run it on a file that may be damaged.
  `grafeno check --recall 100` also estimates the recall of vector
  searches, and `grafeno compact -n NS` (or `--all`) rebuilds a
  namespace's vector graph when the check reports vectors searches
  cannot reach.

```bash
cargo install --path crates/grafeno-server
```

```bash
grafeno serve --db memory.redb --token change-me
```

```bash
grafeno mcp --db ~/.grafeno/memory.redb --namespace me
```

A database file can be open in only one process at a time, so an HTTP server
and an MCP server cannot share a file. Use `--memory` for a throwaway
in-memory database.

`grafeno serve` options (each also from the environment or `--config`):

| Option            | Environment     | Default          |
| ----------------- | --------------- | ---------------- |
| `--config FILE`   | `GRAFENO_CONFIG`| none             |
| `--db FILE`       | `GRAFENO_DB`    | `grafeno.redb`   |
| `--memory`        |                 | off              |
| `--addr ADDR`     | `GRAFENO_ADDR`  | `127.0.0.1:7810` |
| `--token TOKEN`   | `GRAFENO_TOKEN` | none (no auth)   |
| `--no-studio`     |                 | Studio on        |
| `--cors-origin O` (repeatable, `*` for any) | | none   |

## Shell

```bash
grafeno shell --db memory.redb -n vault
```

```bash
grafeno shell --url http://127.0.0.1:7810 --token change-me -n vault
```

```text
vault=> MATCH (p:Person)-[:works_at]->(c)
vault->   RETURN p.name, c.name LIMIT 3;
 p.name | c.name
--------+--------
 Ana    | Acme
 Bob    | Acme
(2 rows)
vault=> \context who works at Acme?
```

Statements are [GQ](../../docs/query-language.md) and end with `;`; lines
that start with `\` are meta-commands (`\?` lists them): `\ns` (and
`\ns create NAME`, `\ns drop NAME --yes`), `\use`,
`\stats`, `\check`, `\rebuild`, `\search`, `\recall`, `\context`,
`\export`, `\import`, `\format table|json`, `\timing`, `\i FILE`, `\q`.
History is kept in `~/.grafeno_history`.

Without a terminal it runs scripts and exits, with an error code if a
statement failed: `-c "…"`, `-f FILE` (`-` for standard input), or a script
piped into it. `--json` prints results as JSON.

```bash
grafeno shell --db memory.redb -n vault -c "MATCH (n) RETURN count(*);"
```

## Administration commands

They open the file directly (stop the server first):

| Command                                   | What it does                                                   |
| ----------------------------------------- | -------------------------------------------------------------- |
| `grafeno namespaces --db F`               | Namespaces with their node and edge counts.                    |
| `grafeno stats --db F [-n NS]...`         | Statistics as JSON: counts per label, texts, vectors.          |
| `grafeno check --db F [-n NS]...`         | Integrity check; exits with an error if anything is wrong.     |
| `grafeno rebuild --db F (-n NS... \| --all)` | Rebuilds indexes and counters from the records.             |
| `grafeno export --db F [-n NS]... [-o FILE]` | JSON Lines export (standard output by default).             |
| `grafeno import --db F FILE`              | Imports an export (`-` reads standard input), all or nothing; `--bulk` loads large exports in chunks. |

```bash
grafeno export --db memory.redb -o backup.jsonl
```

```bash
grafeno import --db restored.redb backup.jsonl
```

| Option        | Environment         | Default            | Command         |
| ------------- | ------------------- | ------------------ | --------------- |
| `--db`        | `GRAFENO_DB`        | `grafeno.redb`     | `serve`, `mcp`  |
| `--memory`    |                     | off                | `serve`, `mcp`  |
| `--addr`      | `GRAFENO_ADDR`      | `127.0.0.1:7810`   | `serve`         |
| `--token`     | `GRAFENO_TOKEN`     | none (no auth)     | `serve`         |
| `--namespace` | `GRAFENO_NAMESPACE` | `default`          | `mcp`           |

## Docker

```bash
docker build -t grafeno .
```

```bash
docker run -p 7810:7810 -v grafeno-data:/data -e GRAFENO_TOKEN=change-me grafeno
```

The image listens on `0.0.0.0:7810` and stores data in `/data`. Set a token
whenever the port is reachable by anything but the application.

## Access

Every request under `/v1` carries `Authorization: Bearer <secret>`, where
the secret is either the server's **token** (`grafeno.toml`, `--token`),
which may do everything, or an **API key**. A key has a name and grants:
a role on a namespace (`vault`), on every namespace starting with a prefix
(`team-*`), or on all (`*`). Roles add up:

| Role | May |
| --- | --- |
| `read` | read, search, recall, run read queries and algorithms |
| `write` | also create and change nodes and facts, merge, archive, touch |
| `admin` | also indexes and constraints, rebuild, describe and drop the namespace, create it |

A key may also be an **owner** (everything, including managing keys) and
may be limited to **its own records**: then it changes or deletes only
nodes and facts it recorded (others' are `403 forbidden`, in GQ too).
Every write by a key records its name: `recorded_by` on nodes and edges,
`n.@recorded_by` in GQ. A key sees only the namespaces it may read: in
`/v1/namespaces`, `SHOW NAMESPACES` and exports.

```bash
grafeno keys create research-agent --grant vault:write --grant "team-*:read" --config grafeno.toml
```

```bash
grafeno keys list --config grafeno.toml
```

```bash
grafeno keys revoke research-agent --config grafeno.toml
```

With `--config`, the commands go through the running server (its address
and token); with `--db`, they open a database file whose server is
stopped. The secret is printed once; only its SHA-256 is stored. Keys are
also managed from the Studio (Administration) and over HTTP
(`/v1/keys`, owners only). A server with neither a token nor keys is open
to anyone who reaches it; its first key must then be an owner, and from
then on every request needs a key.

## HTTP API

All bodies are JSON. Everything under `/v1` needs `Authorization: Bearer
<secret>` (the token or a key) unless the server is open. Errors look like
`{"error": {"code": "not_found", "message": "…"}}`, with codes `bad_request`
(malformed body), `invalid` (rejected input), `invalid_query` (a GQ error),
`query_limit` (a GQ query over its time, reads, rows, memory or cost limit;
see `[query]` in `grafeno.toml`), `not_found`, `conflict` (409: it exists already),
`unauthorized` (401: no valid secret), `forbidden` (403: the key may not do
this), `busy` (503 with `Retry-After`:
too many requests at once, or too many writes waiting) and `internal`.

Writes from concurrent requests are committed together (group commit): each
request still gets its own all-or-nothing result, and a failing write does
not affect the others.

Ids are UUIDs. Times in responses are microseconds since the Unix epoch;
requests accept either that or an ISO 8601 string (`2024-05-01`,
`2024-05-01T10:30:00Z`).

| Method   | Path                                  | Body / query                                            | Returns                       |
| -------- | ------------------------------------- | ------------------------------------------------------- | ----------------------------- |
| `GET`    | `/health`                             |                                                         | `{status, version}`           |
| `GET`    | `/v1/namespaces`                      |                                                         | `[{name, nodes, edges, created_at, description?}]`, empty ones too |
| `POST`   | `/v1/namespaces`                      | `{name, description?}`                                  | the namespace (201); 409 if it exists |
| `GET`    | `/v1/namespaces/{ns}`                 |                                                         | the namespace, or 404         |
| `PATCH`  | `/v1/namespaces/{ns}`                 | `{description}` (`null` removes it)                     | the namespace                 |
| `DELETE` | `/v1/namespaces/{ns}`                 |                                                         | `{nodes, edges}` deleted with it |
| `GET`    | `/v1/backup`                          |                                                         | the whole database file, consistent (owners) |
| `GET`    | `/v1/export`                          | `?ns=a,b` (all by default)                              | JSON Lines                    |
| `POST`   | `/v1/import`                          | JSON Lines (up to 1 GiB); `?bulk=true` loads in chunks  | `{namespaces, nodes, edges}`  |
| `GET`    | `/v1/{ns}/stats`                      |                                                         | statistics                    |
| `POST`   | `/v1/{ns}/compact`                    |                                                         | rebuilds the vector graph (admin): `{vectors, dangling_before, unreachable_before, unreachable_after}` |
| `GET`    | `/v1/{ns}/vectors/recall`             | `?sample=100&ef=64`                                     | `{sample, ef, recall}`: estimated recall@10 |
| `GET`    | `/v1/{ns}/check`                      |                                                         | `{…, problems}`               |
| `POST`   | `/v1/{ns}/rebuild`                    |                                                         | rebuild report                |
| `POST`   | `/v1/{ns}/nodes`                      | node (with `id`: upsert)                                | `{id, created}`               |
| `GET`    | `/v1/{ns}/nodes`                      | `?label=&after=&limit=`                                 | `{items, next}`               |
| `GET`    | `/v1/{ns}/edges`                      | `?label=&after=&limit=`                                 | `{items, next}`               |
| `GET`    | `/v1/{ns}/nodes/{id}`                 |                                                         | node with `text` and `memory` |
| `PUT`    | `/v1/{ns}/nodes/{id}`                 | node                                                    | `{id, created}`               |
| `PATCH`  | `/v1/{ns}/nodes/{id}`                 | patch                                                   | node with `text`              |
| `DELETE` | `/v1/{ns}/nodes/{id}`                 |                                                         | 204                           |
| `GET`    | `/v1/{ns}/nodes/{id}/neighbors`       | `?direction=&label=a,b&valid_at=&known_at=`             | `[{node, edge}]`              |
| `POST`   | `/v1/{ns}/edges`                      | edge                                                    | `{id}`                        |
| `GET`    | `/v1/{ns}/edges/{id}`                 |                                                         | edge                          |
| `DELETE` | `/v1/{ns}/edges/{id}`                 |                                                         | 204                           |
| `POST`   | `/v1/{ns}/edges/{id}/invalidate`      | `{at}`                                                  | edge                          |
| `POST`   | `/v1/{ns}/edges/{id}/retract`         |                                                         | edge                          |
| `POST`   | `/v1/{ns}/k-hop`                      | `{start, depth, direction, filter}`                     | `[{node, hops}]`              |
| `POST`   | `/v1/{ns}/path`                       | `{from, to, max_depth, direction, filter}`              | `{path}` (or `null`)          |
| `POST`   | `/v1/{ns}/subgraph`                   | `{seeds, depth, direction, filter}`                     | `{nodes, edges}`              |
| `POST`   | `/v1/{ns}/search/text`                | `{query, limit}`                                        | `[{node, score}]`             |
| `POST`   | `/v1/{ns}/search/vector`              | `{embedding, limit, ef}`                                | `[{node, score}]`             |
| `POST`   | `/v1/{ns}/recall`                     | recall query                                            | `[{node, score, signals}]`    |
| `POST`   | `/v1/{ns}/context`                    | recall query + `{budget, fact_share, max_facts, facts}` | `{items, facts, tokens, rendered}` |
| `POST`   | `/v1/{ns}/batch`                      | `{ops}`                                                 | `{results, refs}`             |
| `POST`   | `/v1/{ns}/query`                      | `{query, params?}` ([GQ](../../docs/query-language.md)) | `{columns, rows, stats}`      |
| `GET`    | `/v1/whoami`                          |                                                         | the caller: `{name, owner, grants, own_only}` |
| `GET`    | `/v1/keys`                            |                                                         | the keys, never their secrets (owners) |
| `POST`   | `/v1/keys`                            | `{name, grants: [{namespace, role}], owner?, own_only?, description?}` | `{key, secret}` (201; the secret only here) |
| `DELETE` | `/v1/keys/{name}`                     |                                                         | 204                           |
| `GET`    | `/v1/algorithms`                      |                                                         | `[{name, arguments, columns, description}]` |
| `POST`   | `/v1/{ns}/algo/{name}`                | `{args?, settings?, write?, column?}`                   | `{columns, rows, stats}`      |
| `GET`    | `/v1/{ns}/resolve`                    | `?name=&label=`                                         | nodes with that name or alias |
| `GET`    | `/v1/{ns}/duplicates`                 | `?label=&min_score=&vectors=&limit=`                    | `[{a, b, score, reasons}]`    |
| `POST`   | `/v1/{ns}/merge`                      | `{keep, merge}`                                         | `{kept, merged, edges_moved, edges_dropped}` |
| `POST`   | `/v1/{ns}/touch`                      | `{ids}`                                                 | 204                           |
| `GET`    | `/v1/{ns}/weak`                       | `?half_life_days=&threshold=&label=&limit=`             | `[{node, strength, memory}]`  |
| `POST`   | `/v1/{ns}/archive`                    | `{ids}`                                                 | `{archived}`                  |
| `GET`    | `/v1/{ns}/consolidation`              | `?older_than_days=&min_size=&labels=a,b&limit=`         | `[{entity, memories}]`        |
| `POST`   | `/v1/{ns}/summaries`                  | `{text, sources, about?, name?, labels?, embedding?, archive_sources?}` | `{id}` (201)  |

- **Graph algorithms** (`pageRank`, `louvain`, `shortestPath`, … see
  [GQ](../../docs/query-language.md#graph-algorithms)): `args` are the
  arguments before the settings (node ids, `k`), `settings` the settings
  map (`{"labels": "Person", "weight": "km"}`). Rows hold nodes as `{id,
  labels, name}`. With `write: "rank"` (or `"@embedding"`) the result is
  stored on each node instead, from `column` (the one after `node` by
  default), and the answer is the number of nodes written.
- **Listings** return up to `limit` items (default 100, at most 1000) in id
  order; when `next` is set, pass it as `after` to get the following page.
- **node**: `{labels, props?, sources?, text?, embedding?}`. `PUT` and upserts
  replace the whole node; a missing `text` or `embedding` is removed, edges
  are kept.
- **patch**: `{props?, add_labels?, remove_labels?, text?, embedding?,
  importance?}`. `props` is merged (`null` removes a key); `text: null` or
  `embedding: null` removes them.
- **edge**: `{from, label, to, props?, sources?, valid_from?, valid_to?}`.
- **filter**: `{labels?, valid_at?, known_at?}`. Without `known_at`, retracted
  edges are left out.
- **recall query**: `{text?, embedding?, limit?, labels?, candidates?,
  expand?, ranking?, include_archived?, touch?}`, where `expand` is `true`
  (default), `false`, or `{depth, direction, filter, decay, seeds}`;
  `ranking` is `true` (default: importance and recency weigh in), `false`, or
  `{importance_weight, recency_weight, half_life_days}`; archived memories
  are left out unless `include_archived`; `touch` records an access to each
  result.
- **memory** (in node responses): `{importance, accesses, last_access}`.

### Batches

A batch runs in one transaction: if an operation fails, nothing is written
and the error message starts with `operation N:`. Operations can name the
nodes they create with `ref` (`"$name"`) and later operations can use that
name wherever they take a node id. Operations: `create_node`, `upsert_node`
(`node.id` required), `update_node` (`{id, patch}`), `delete_node`,
`create_edge`, `delete_edge`, `delete_edges_from` (`{from, label?}`),
`invalidate_edge` (`{id, at}`) and `retract_edge`.

Syncing a note and replacing its links in one request:

```json
{ "ops": [
  { "op": "upsert_node", "ref": "$note",
    "node": { "id": "0190f5c2-7a3e-7cde-8f01-00000000000a", "labels": ["Note"],
              "props": { "title": "Rust" }, "text": "The borrow checker…" } },
  { "op": "delete_edges_from", "from": "$note", "label": "links_to" },
  { "op": "create_edge", "edge": { "from": "$note", "label": "links_to",
                                   "to": "0190f5c2-7a3e-7cde-8f01-00000000000b" } }
] }
```

## MCP

`grafeno mcp` speaks MCP (protocol versions 2025-06-18, 2025-03-26 and
2024-11-05) over stdin and stdout, and logs to stderr. Its tools:

| Tool       | What it does                                                                   |
| ---------- | ------------------------------------------------------------------------------ |
| `remember` | Stores a memory (text, optional name, labels, importance and links). A known `name` updates that entity instead. |
| `recall`   | Hybrid search; returns ids, names, snippets and why each result matched. Records accesses. |
| `context`  | Relevant memories and facts as compact text within a token budget.             |
| `relate`   | Records a fact (edge) between two nodes, with optional valid dates.           |
| `update`   | Replaces a node's text, renames it, merges properties, changes labels or importance. |
| `forget`   | Invalidates (default), retracts or deletes a fact; deletes a node.            |
| `get_node` | A node with its text, properties and connecting facts.                        |
| `query`    | A read-only [GQ](../../docs/query-language.md) statement; at most 200 rows.   |
| `merge`    | Folds a duplicate node into another one.                                       |
| `maintain` | Lists likely duplicates, weak memories and groups ready to consolidate.        |
| `consolidate` | Stores a summary of several memories and archives them.                     |

Every tool takes an optional `namespace`, which defaults to `--namespace`.

The MCP server opens the database file itself, so it cannot keep out the
user who starts it, but it fences the agent it serves:

| Flag | Effect |
| --- | --- |
| `--allow vault,team-*` | the namespaces the agent may use (all by default) |
| `--read-only` | only the tools that read are offered (and recall records no accesses) |
| `--as NAME` | its writes are recorded as `NAME` (`@recorded_by`) |
| `--own-only` | with `--as`: it may change only what it recorded |

Claude Code:

```bash
claude mcp add grafeno -- grafeno mcp --db ~/.grafeno/memory.redb --namespace me
```

Claude Desktop (`claude_desktop_config.json`) or other clients:

```json
{
  "mcpServers": {
    "grafeno": {
      "command": "grafeno",
      "args": ["mcp", "--db", "/path/to/memory.redb", "--namespace", "me"]
    }
  }
}
```
