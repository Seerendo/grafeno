// End-to-end test of a cluster: three `grafeno serve` processes with
// `[cluster]` settings. The client knows all three; it reads its own
// writes on any member, a keyed write runs once, and when the member it
// talks to dies it goes on with the others. Build the binary first
// (`cargo build -p grafeno-server`) or point GRAFENO_BIN to it.

import { type ChildProcess, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:net";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import assert from "node:assert/strict";

import { Grafeno } from "../src/index.ts";

const binary =
  process.env.GRAFENO_BIN ??
  fileURLToPath(
    new URL(`../../../target/debug/grafeno${process.platform === "win32" ? ".exe" : ""}`, import.meta.url),
  );
const token = "cluster-test-token";
const dir = mkdtempSync(join(tmpdir(), "grafeno-ts-cluster-"));
const members: { id: number; api: string; process?: ChildProcess }[] = [];

/** A free port: bound, read, released. */
async function freePort(): Promise<number> {
  const server = createServer();
  server.listen(0, "127.0.0.1");
  await once(server, "listening");
  const port = (server.address() as { port: number }).port;
  server.close();
  await once(server, "close");
  return port;
}

async function waitFor<T>(what: string, check: () => Promise<T | undefined>, ms = 30_000): Promise<T> {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    const value = await check().catch(() => undefined);
    if (value !== undefined) return value;
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`timed out waiting for ${what}`);
}

before(async () => {
  const ports = [];
  for (let i = 0; i < 6; i++) ports.push(await freePort());
  const peers = [1, 2, 3]
    .map((id) => `{ id = ${id}, addr = "127.0.0.1:${ports[id - 1]}", api = "127.0.0.1:${ports[id + 2]}" }`)
    .join(", ");
  for (const id of [1, 2, 3]) {
    const config = join(dir, `member-${id}.toml`);
    writeFileSync(
      config,
      [
        `db = "${join(dir, `member-${id}.redb`).replaceAll("\\", "/")}"`,
        `addr = "127.0.0.1:${ports[id + 2]}"`,
        `token = "${token}"`,
        "studio = false",
        "",
        "[cluster]",
        `id = ${id}`,
        `addr = "127.0.0.1:${ports[id - 1]}"`,
        'secret = "s3cret"',
        "voters = [1, 2, 3]",
        "tick_ms = 20",
        `peers = [${peers}]`,
        "",
      ].join("\n"),
    );
    const child = spawn(binary, ["serve", "--config", config], { stdio: "ignore" });
    members.push({ id, api: `http://127.0.0.1:${ports[id + 2]}`, process: child });
  }
});

after(async () => {
  for (const member of members) {
    if (member.process && member.process.exitCode === null) {
      member.process.kill();
      await once(member.process, "exit");
    }
  }
  rmSync(dir, { recursive: true, force: true });
});

test("a client follows a cluster", async () => {
  // Wait for a leader.
  await waitFor("a leader", async () => {
    for (const member of members) {
      const status = await new Grafeno({ url: member.api, token }).cluster();
      if (status.role === "leader") return member;
    }
    return undefined;
  });
  // Talk to the members in an order that starts on a follower.
  const order = [...members].reverse().map((member) => member.api);
  const client = new Grafeno({ url: order, token });
  const ns = client.namespace("c");
  for (let i = 0; i < 5; i++) await ns.query("CREATE (:Note {i: $i})", { i });
  assert.ok(client.position > 0, "the client saw a position");
  // Its own writes are visible on the member it reads from, at once.
  assert.equal((await ns.query("MATCH (n:Note) RETURN count(n)")).rows[0][0], 5);
  // A keyed write runs once.
  for (let i = 0; i < 3; i++) {
    await ns.query("CREATE (:Note {i: 100})", {}, { idempotencyKey: "k-100" });
  }
  const linearizable = await ns.query("MATCH (n:Note) RETURN count(n)", {}, { consistency: "linearizable" });
  assert.equal(linearizable.rows[0][0], 6);
  // The member in use dies: the client moves on.
  const used = members.find((member) => member.api === client.url)!;
  used.process!.kill();
  await once(used.process!, "exit");
  await waitFor("a write after the member died", () => ns.query("CREATE (:Note {i: 7})").then(() => true));
  assert.notEqual(client.url, used.api);
  assert.equal((await ns.query("MATCH (n:Note) RETURN count(n)")).rows[0][0], 7);
});
