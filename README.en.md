# Grafeno

<p align="center">
  <img src="docs/assets/readme-banner-en.svg" width="1440" alt="Grafeno. Memory, connected. Rust graph engine, Tauri desktop and MCP integration.">
</p>

<p align="center">
  <a href="README.md">Español</a> · <strong>English</strong><br>
  <a href="#start-with-the-desktop">Get started</a> ·
  <a href="docs/install.md">Installation</a> ·
  <a href="docs/query-language.md">GQ queries</a> ·
  <a href="CONTRIBUTING.en.md">Contribute</a>
</p>

Grafeno is a **Rust graph engine for agent memory and knowledge**. It connects
entities, documents and facts; preserves provenance and history; and combines
text, vector and graph retrieval to assemble context. Its first client is
[Grafito](https://github.com/Seerendo/grafito).

On the desktop, open a local database with the engine inside the Tauri
application. Grafeno can also run as a Rust library, HTTP server or MCP server
for agents.

## What you can do

| Capability | Purpose |
| --- | --- |
| **Connect knowledge** | Nodes with labels and properties, directed relationships and namespaces to organize data. |
| **Query the past** | Distinguish when a fact was valid from when the system knew it using `VALID AT` and `KNOWN AT`. |
| **Retrieve context** | Combine BM25 text search, HNSW vectors and graph expansion; pack context into a token budget. |
| **Maintain memory** | Resolve entities, review possible duplicates, record reversible merges and summarize communities without a model. |
| **Explore and query** | Visualize relationships, edit information and run GQ, a Cypher-like language. |
| **Operate the database** | Use keys with permissions, backups, import and export, integrity checks and index rebuilds. |

## Start with the desktop

**Grafeno Desktop** creates or opens `.redb` files, keeps recent databases and
connects to servers. Local mode runs the engine inside the process: it needs no
service and opens no port.

For Windows and Linux packages, see the [installation guide](docs/install.md)
and the [desktop documentation](apps/desktop/README.md).

To run from source, prepare the [development requirements](CONTRIBUTING.en.md#requirements)
first. From the repository root:

```bash
npm ci --prefix studio
npm run build --prefix studio
cargo run --locked -p grafeno-desktop
```

A `.redb` file can be open in only one process at a time. Close the database in
the desktop application before opening the same file with the server or shell,
and vice versa.

## A query, relationships and history

With the `grafeno` binary installed, open a local shell:

```bash
grafeno shell --db ejemplo.redb -n demo
```

Create a person and a relationship with a starting date:

```cypher
CREATE (a:Person {name: 'Ana'})
  -[:works_at {@valid_from: '2024-01-01'}]->
  (h:Organization {name: 'Hospital Central'})
RETURN a.name, h.name;
```

Query what was valid on a particular date:

```cypher
MATCH (p:Person)-[r:works_at]->(o:Organization)
VALID AT '2024-06-01'
RETURN p.name, o.name, r.@valid_from;
```

GQ also includes aggregates, indexes and algorithms for paths, centrality,
communities and similarity. See the [GQ reference](docs/query-language.md).

## Integrate applications and agents

| Access mode | Entry point |
| --- | --- |
| Local desktop or server connection | [Tauri application](apps/desktop/README.md) |
| HTTP service, MCP server and shell | [Grafeno server](crates/grafeno-server/README.md) |
| JavaScript and TypeScript applications | [TypeScript client](clients/typescript/README.md) |
| Embedded Rust library | [`grafeno`](crates/grafeno/src/lib.rs) and [`grafeno-memory`](crates/grafeno-memory/src/lib.rs) |

To start MCP with its own database after installing the binary:

```bash
grafeno mcp --db agente.redb
```

To try the HTTP server with temporary in-memory data:

```bash
grafeno serve --memory
```

It listens on `127.0.0.1:7810` by default. Studio is at
<http://127.0.0.1:7810/studio/> when its build is embedded in the binary.
Data in `--memory` is lost when the process exits. For persistent, authenticated
configuration, follow the [server guide](docs/install.md#settings).

## Repository structure

| Directory | Responsibility |
| --- | --- |
| `crates/grafeno-core` | Nodes, edges, identifiers and time. |
| `crates/grafeno` | redb storage, transactions, indexes, traversals and algorithms. |
| `crates/grafeno-memory` | Hybrid retrieval, context, extraction and memory maintenance. |
| `crates/grafeno-query` | GQ parser, planning and execution. |
| `crates/grafeno-server` | HTTP API, MCP, shell and administration. |
| `studio` | Interface shared by the browser and desktop application. |
| `apps/desktop` | Tauri window, local files, menus and engine bridge. |
| `clients/typescript` | HTTP client for applications. |

## Development and tests

The [English contribution guide](CONTRIBUTING.en.md) and its
[Spanish version](CONTRIBUTING.md) cover prerequisites, desktop builds and an
alternative that reduces temporary storage usage.

To check the engine without installing the Tauri graphical libraries:

```bash
cargo test --workspace --locked --exclude grafeno-desktop
```

The [contribution workflow](.github/workflows/ci.yml) checks the Studio and
TypeScript client, engine and server, client integration, Tauri bridge tests
and the desktop build on Linux. Visual interaction and installers need
additional checks.

## Design, performance and scope

- [Design and decisions](DESIGN.md): architecture, storage and temporal semantics.
- [Performance](docs/performance.md) and [benchmarks](docs/benchmarks.md): methods, data and limits of project measurements.
- [Development history](docs/development-history.md): the originally documented phases in detail.

Embeddings are provided by the client. Context uses an estimated token count by
default; the Rust library accepts a model-specific counter. The engine supports
multiple readers and one writer; distribution and sharding are not implemented.
The license remains an open decision in the design.

Created by [Seerendo](https://github.com/Seerendo). The visual identity depicts
three fused carbon rings from a graphene lattice.
