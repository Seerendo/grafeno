// End-to-end test: starts the real `grafeno serve` binary and drives it
// through the client. Build it first (`cargo build -p grafeno-server`) or
// point GRAFENO_BIN to it.

import { type ChildProcess, spawn } from "node:child_process";
import { once } from "node:events";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import assert from "node:assert/strict";

import { Grafeno, GrafenoError } from "../src/index.ts";

const binary =
  process.env.GRAFENO_BIN ??
  fileURLToPath(
    new URL(`../../../target/debug/grafeno${process.platform === "win32" ? ".exe" : ""}`, import.meta.url),
  );
const token = "test-token";
let server: ChildProcess;
let url: string;

before(async () => {
  server = spawn(binary, ["serve", "--memory", "--addr", "127.0.0.1:0", "--token", token], {
    stdio: ["ignore", "ignore", "pipe"],
  });
  url = await new Promise<string>((resolve, reject) => {
    let output = "";
    server.stderr!.on("data", (chunk: Buffer) => {
      output += chunk.toString();
      const match = output.match(/listening on (http:\/\/\S+)/);
      if (match) resolve(match[1]);
    });
    server.once("exit", (code) => reject(new Error(`grafeno exited with ${code}: ${output}`)));
  });
});

after(async () => {
  server.kill();
  await once(server, "exit");
});

test("health and auth", async () => {
  const grafeno = new Grafeno({ url, token });
  assert.equal((await grafeno.health()).status, "ok");
  const anonymous = new Grafeno({ url }).namespace("vault");
  await assert.rejects(anonymous.listNodes(), (error: unknown) => {
    assert.ok(error instanceof GrafenoError);
    assert.equal(error.status, 401);
    assert.equal(error.code, "unauthorized");
    return true;
  });
});

test("sync a note like Grafito would", async () => {
  const vault = new Grafeno({ url, token }).namespace("user-1");
  const noteId = "0190f5c2-7a3e-7cde-8f01-00000000000a";
  const otherId = "0190f5c2-7a3e-7cde-8f01-00000000000b";
  await vault.putNode(otherId, { labels: ["Note"], props: { title: "Ownership" }, text: "Each value has one owner" });

  const sync = (text: string, links: string[]) =>
    vault.batch([
      { op: "upsert_node", ref: "$note", node: { id: noteId, labels: ["Note"], props: { title: "Rust" }, text } },
      { op: "delete_edges_from", from: "$note", label: "links_to" },
      ...links.map((to) => ({ op: "create_edge" as const, edge: { from: "$note", label: "links_to", to } })),
    ]);

  const first = await sync("The borrow checker, see [[Ownership]]", [otherId]);
  assert.equal(first.refs.$note, noteId);
  assert.equal((await vault.neighbors(noteId, { direction: "out" })).length, 1);

  // Saving again with no links replaces them; the note keeps its id.
  const second = await sync("The borrow checker", []);
  assert.equal(second.results[1].deleted, 1);
  assert.equal((await vault.neighbors(noteId, { direction: "out" })).length, 0);
  assert.equal((await vault.getNode(noteId)).text, "The borrow checker");
});

test("recall and context", async () => {
  const memory = new Grafeno({ url, token }).namespace("agent");
  const { refs } = await memory.batch([
    { op: "create_node", ref: "$ana", node: { labels: ["Person"], props: { name: "Ana" } } },
    {
      op: "create_node",
      ref: "$pref",
      node: { labels: ["Preference"], text: "Prefers answers in Spanish", embedding: [1, 0, 0] },
    },
    { op: "create_edge", edge: { from: "$pref", label: "about", to: "$ana", valid_from: "2026-01-01" } },
  ]);

  const recalled = await memory.recall({ text: "answers in Spanish", embedding: [0.9, 0.1, 0] });
  assert.equal(recalled[0].node.id, refs.$pref);
  assert.deepEqual(
    recalled[0].signals.map((signal) => signal.kind),
    ["text", "vector"],
  );
  assert.equal(recalled[1].node.id, refs.$ana);

  const context = await memory.context({ text: "Spanish", budget: 300 });
  assert.match(context.rendered, /^## Memory\n\[1\] \(Preference\)\nPrefers answers in Spanish\n/);
  assert.match(context.rendered, /- “Prefers answers in Spanish” about Ana \(from 2026-01-01\)/);
  assert.ok(context.tokens <= 300);

  const edge = (await memory.neighbors(refs.$ana, { direction: "in" }))[0].edge;
  const invalidated = await memory.invalidateEdge(edge.id, "2026-06-01T00:00:00Z");
  assert.equal(invalidated.valid_to, Date.UTC(2026, 5, 1) * 1000);
  assert.equal((await memory.neighbors(refs.$ana, { validAt: "2026-07-01" })).length, 0);
  assert.equal((await memory.neighbors(refs.$ana, { validAt: "2026-03-01" })).length, 1);
});

test("errors carry status and code", async () => {
  const vault = new Grafeno({ url, token }).namespace("user-1");
  await assert.rejects(vault.getNode("0190f5c2-7a3e-7cde-8f01-0000000000ff"), { status: 404, code: "not_found" });
  await assert.rejects(vault.createNode({ labels: [] }), { status: 400, code: "invalid" });
});
