// The contract between this client and the engine (phase 15.3): the client
// is run against the real `grafeno serve` binary of every engine version it
// supports, and uses only what each engine says it can do.
//
// GRAFENO_CONTRACT_BINS lists the binaries (separated like PATH: `:` on
// Linux, `;` on Windows): the engine being built and the previous release.
// Without it, GRAFENO_BIN or the debug build of this repository.

import { type ChildProcess, spawn } from "node:child_process";
import { once } from "node:events";
import { delimiter } from "node:path";
import { fileURLToPath } from "node:url";
import { test } from "node:test";
import assert from "node:assert/strict";

import { API_VERSIONS, type About, Grafeno, GrafenoError, LEGACY_CAPABILITIES } from "../src/index.ts";

const defaultBinary =
  process.env.GRAFENO_BIN ??
  fileURLToPath(
    new URL(`../../../target/debug/grafeno${process.platform === "win32" ? ".exe" : ""}`, import.meta.url),
  );
const binaries = (process.env.GRAFENO_CONTRACT_BINS ?? defaultBinary)
  .split(delimiter)
  .filter((path) => path !== "");
const token = "contract-token";

async function start(binary: string): Promise<{ server: ChildProcess; url: string }> {
  const server = spawn(binary, ["serve", "--memory", "--addr", "127.0.0.1:0", "--token", token], {
    stdio: ["ignore", "ignore", "pipe"],
  });
  const url = await new Promise<string>((resolve, reject) => {
    let output = "";
    server.stderr!.on("data", (chunk: Buffer) => {
      output += chunk.toString();
      const match = output.match(/listening on (http:\/\/\S+)\r?\n/);
      if (match) resolve(match[1]);
    });
    server.once("exit", (code) => reject(new Error(`${binary} exited with ${code}: ${output}`)));
  });
  return { server, url };
}

for (const binary of binaries) {
  test(`the client against ${binary}`, async (t) => {
    const { server, url } = await start(binary);
    try {
      const grafeno = new Grafeno({ url, token });
      const about: About = await grafeno.connect();
      t.diagnostic(`engine ${about.version}, API ${about.api.version}${about.legacy ? " (legacy)" : ""}`);
      assert.equal(about.product, "grafeno");
      assert.ok(about.api.supported.some((version) => API_VERSIONS.includes(version)));
      assert.ok(grafeno.supports("nodes"));
      // Every capability is one the client knows (or a newer one it can ignore).
      for (const capability of about.capabilities) assert.equal(typeof capability, "string");

      const vault = grafeno.namespace("contract");
      const { id: a } = await vault.createNode({ labels: ["Person"], props: { name: "Ada" }, text: "wrote the first program" });
      const { id: b } = await vault.createNode({ labels: ["Engine"], props: { name: "Analytical" } });
      const edge = await vault.createEdge({ from: a, to: b, label: "worked_on" });
      assert.ok(edge.id);
      assert.equal((await vault.getNode(a)).props.name, "Ada");
      assert.equal((await vault.listNodes()).items.length, 2);

      if (grafeno.supports("search.text")) {
        const hits = await vault.searchText("program", 5);
        assert.equal(hits[0].node, a);
      }
      if (grafeno.supports("query")) {
        const result = await vault.query("MATCH (p:Person)-[:worked_on]->(e) RETURN p.name, e.name");
        assert.deepEqual(result.rows, [["Ada", "Analytical"]]);
      }
      if (grafeno.supports("traversal")) {
        const hop = await vault.kHop(a, { depth: 1, direction: "out" });
        assert.ok(JSON.stringify(hop).includes(b));
      }
      if (grafeno.supports("recall")) {
        const recalled = await vault.recall({ text: "first program", limit: 3 });
        assert.equal(recalled[0].node.id, a);
      }
      if (grafeno.supports("export") && grafeno.supports("import")) {
        const dump = await grafeno.export(["contract"]);
        const other = new Grafeno({ url, token });
        await other.namespace("contract").deleteNode(b);
        await other.import(dump);
        assert.equal((await vault.getNode(b)).props.name, "Analytical");
      }
      if (grafeno.supports("backup")) {
        const bytes = await grafeno.backup();
        assert.ok(bytes.byteLength > 0);
      }
      if (grafeno.supports("accounts")) {
        await grafeno.createAccount({ name: "reader", kind: "user", password: "a-long-passw0rd!", grants: [{ namespace: "contract", role: "read" }] });
        const session = new Grafeno({ url });
        await session.signIn("reader", "a-long-passw0rd!");
        assert.equal((await session.namespace("contract").getNode(a)).id, a);
      }
      if (grafeno.supports("settings")) {
        const settings = await grafeno.settings();
        assert.equal(settings.token, true);
        assert.ok(!JSON.stringify(settings).includes(token));
      }
    } finally {
      server.kill();
      await once(server, "exit");
    }
  });
}

// Without a server: what the client does with engines it cannot use, and
// with engines from before `/v1/about`.
function fakeEngine(routes: Record<string, [number, unknown]>): typeof fetch {
  return async (input) => {
    const path = new URL(String(input instanceof Request ? input.url : input)).pathname;
    const [status, body] = routes[path] ?? [404, { error: { code: "not_found", message: "no route" } }];
    return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
  };
}

test("an engine that speaks no API version of this client is refused", async () => {
  const grafeno = new Grafeno({
    url: "http://engine.test",
    fetch: fakeEngine({
      "/v1/about": [200, {
        product: "grafeno", version: "9.0.0", api: { version: 9, supported: [8, 9] }, layout: 40,
        capabilities: ["nodes"], auth: { token: true, accounts: true, open: false, password_sign_in: true }, cluster: null,
      }],
    }),
  });
  await assert.rejects(grafeno.connect(), (error: unknown) => {
    assert.ok(error instanceof GrafenoError);
    assert.equal(error.code, "incompatible");
    assert.equal(error.status, 0);
    assert.match(error.message, /9\.0\.0/);
    return true;
  });
});

test("an engine from before /v1/about is told from /health", async () => {
  const grafeno = new Grafeno({
    url: "http://engine.test",
    fetch: fakeEngine({ "/health": [200, { status: "ok", version: "0.1.0" }] }),
  });
  assert.equal(grafeno.supports("nodes"), false);
  const about = await grafeno.connect();
  assert.equal(about.legacy, true);
  assert.equal(about.version, "0.1.0");
  assert.deepEqual(about.capabilities, [...LEGACY_CAPABILITIES]);
  assert.ok(grafeno.supports("audit"));
  assert.equal(grafeno.supports("settings"), false);
  assert.equal(grafeno.engine?.version, "0.1.0");
});

test("newer capabilities an engine reports turn on what they name, unknown ones are ignored", async () => {
  const grafeno = new Grafeno({
    url: "http://engine.test",
    fetch: fakeEngine({
      "/v1/about": [200, {
        product: "grafeno", version: "1.4.0", api: { version: 2, supported: [1, 2] }, layout: 20,
        capabilities: ["nodes", "teleport"], auth: { token: false, accounts: false, open: true, password_sign_in: true }, cluster: { id: 2, group: 0 },
      }],
    }),
  });
  const about = await grafeno.connect();
  assert.equal(about.cluster?.id, 2);
  assert.ok(grafeno.supports("teleport"));
  assert.equal(grafeno.supports("audit"), false);
});
