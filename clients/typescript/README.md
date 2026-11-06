# grafeno-client

TypeScript client for the Grafeno HTTP API (`grafeno serve`). It has no
dependencies and uses the global `fetch`.

```ts
import { Grafeno } from "grafeno-client";

const grafeno = new Grafeno({ url: "http://127.0.0.1:7810", token: process.env.GRAFENO_TOKEN });
const vault = grafeno.namespace("vault-123");

await vault.batch([
  { op: "upsert_node", ref: "$note", node: { id: noteId, labels: ["Note"], props: { title }, text } },
  { op: "delete_edges_from", from: "$note", label: "links_to" },
  ...linkedIds.map((to) => ({ op: "create_edge" as const, edge: { from: "$note", label: "links_to", to } })),
]);

const context = await vault.context({ text: question, budget: 1500 });
prompt += context.rendered;

// GQ queries and graph algorithms.
const people = await vault.query("MATCH (p:Person) WHERE p.age > $age RETURN p.name", { age: 30 });
const ranked = await vault.algorithm("pageRank", { settings: { labels: "Person" } });
await vault.algorithm("louvain", { write: "community" });
```

Field names follow the API (snake_case); see
[the server README](../../crates/grafeno-server/README.md) for every endpoint.
Errors are thrown as `GrafenoError`, with the HTTP `status` and the API's
`code`.

## Development

```bash
npm run build
```

The end-to-end tests start the real server binary, so build it first:

```bash
cargo build -p grafeno-server
```

```bash
npm test
```

## Read nodes and edges together

```ts
const records = await vault.readMany({ nodes: [noteId, otherId], edges: [edgeId] });
```

One request reads one snapshot, preserving input order and duplicate IDs. Missing
records are `null`; nodes include the same text and memory fields as `getNode`.
The server accepts at most 512 IDs across both lists and requires read access to
that namespace. This is a read operation, separate from the writing `batch` API.
See the [method and measurements](../../docs/read-many.md) or [Español](../../docs/lecturas-por-lotes.md).

## Expand neighborhoods together

```ts
const around = await vault.neighborhoods([noteId, otherId], {
  filter: { valid_at: "2026-06-01" },
});
```

Up to eight roots return separate depth-one subgraphs in their original order,
using one snapshot. A missing root fails the batch, matching `subgraph`.
Studio uses this operation for graph expansion. See [English](../../docs/graph-expansion.md)
or [Español](../../docs/expansion-grafo.md) for compatibility and measurements.
