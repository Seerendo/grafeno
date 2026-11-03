# Grafeno Studio

The web interface of Grafeno: overview, explorer, graph view with time
travel, GQ query editor, search and context lab, memory upkeep (duplicates,
weak memories, consolidation) and administration. It is
built into `grafeno serve` at <http://127.0.0.1:7810/studio/>.

## Development

```bash
npm ci
```

```bash
npm run dev
```

`npm run dev` serves it with live reload and forwards `/v1` and `/health`
to a `grafeno serve` on `127.0.0.1:7810` (start one first, for example
`grafeno serve --memory`).

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
