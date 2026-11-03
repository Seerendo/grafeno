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
