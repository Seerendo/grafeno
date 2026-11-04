# Grafeno Studio

The web interface of Grafeno, built into `grafeno serve` at
<http://127.0.0.1:7810/studio/>. Everything can be done from it, without a
terminal:

- **Overview**: namespaces (empty ones too, with their creation date),
  counts per label, quick actions. The namespace menu at the top switches
  between namespaces or creates a new one, and says which it did; creating
  a name that exists offers to switch to it instead.
- **Explorer**: filter by label, name or text; create nodes; edit names,
  labels, properties (typed: text, number, yes/no, JSON), text and
  importance in place; add, end, retract or delete facts; select several
  nodes to archive or delete them.
- **Graph**: draw the graph around search results or everything, create
  nodes, connect two nodes by clicking them, inspect and end facts, travel
  in time with “valid at”, color nodes by community (Louvain, label
  propagation, connected components) and size them by centrality
  (PageRank, degree, betweenness), computed over the whole namespace.
- **Scripts**: a GQ workspace like a SQL editor. Several scripts in tabs
  (kept in the browser, opened from and saved to `.gq` files), highlighting
  and completion (keywords, functions, the namespace's labels,
  pseudo-properties), a schema panel with snippets, Ctrl+Enter to run the
  statement under the cursor or the selection, F5 to run the whole script
  (each statement in its own transaction, stopping at the first error, whose
  line is marked). Results per statement as a table, JSON, graph, bar or
  line chart, or the `PROFILE` steps, with CSV and JSON export, and a messages log with times and changes.
- **Search**: full text, recall with its signals, context within a token
  budget.
- **Memory**: duplicates to merge, weak memories to archive, groups to
  consolidate.
- **Administration**: indexes, check, rebuild, export, import, and
  deleting the namespace with everything in it (after a confirmation).

Ctrl+K opens a command palette (views, actions, namespaces and nodes).

The look is "graph lab": dark first (light, or the system's choice, from
the switch at the bottom of the rail), a floating rail and command bar in
glass over the canvas, Space Grotesk for the interface and JetBrains Mono
for data and code. Both typefaces are bundled (SIL Open Font License), so
the Studio needs no network beyond its server.

## Development

```bash
npm ci
```

```bash
npm run dev
```

`npm run dev` serves it with live reload and forwards `/v1` and `/health`
to a `grafeno serve` on `127.0.0.1:7810` (start one first, for example
`grafeno serve --memory`), or to the server in `GRAFENO_URL`.

```bash
npm run build
```

`npm run build` writes `dist/`. Building `grafeno-server` afterwards embeds
it in the `grafeno` binary (`crates/grafeno-server/build.rs`); without
`dist/`, the binary builds anyway and `/studio/` explains how to add it.

The build uses relative paths and hash routing, so `dist/` can also be
hosted on any static server. Point it at a Grafeno server started with
`--cors-origin <that origin>`.

It calls the API through the TypeScript client in `../clients/typescript`.
