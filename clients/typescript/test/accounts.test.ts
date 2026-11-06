// Phase 14.1 end to end: accounts and sessions through the client, against
// the real `grafeno serve`. Build it first (`cargo build -p grafeno-server`)
// or point GRAFENO_BIN to it.

import { type ChildProcess, spawn } from "node:child_process";
import { once } from "node:events";
import { fileURLToPath } from "node:url";
import { after, before, test } from "node:test";
import assert from "node:assert/strict";

import { Grafeno, GrafenoError, type SessionTokens } from "../src/index.ts";

const binary =
  process.env.GRAFENO_BIN ??
  fileURLToPath(new URL(`../../../target/debug/grafeno${process.platform === "win32" ? ".exe" : ""}`, import.meta.url));
const token = "accounts-test-token";
let server: ChildProcess;
let url: string;

before(async () => {
  server = spawn(binary, ["serve", "--memory", "--addr", "127.0.0.1:0", "--token", token, "--no-studio"], {
    stdio: ["ignore", "ignore", "pipe"],
  });
  url = await new Promise<string>((resolve, reject) => {
    let output = "";
    server.stderr!.on("data", (chunk: Buffer) => {
      output += chunk.toString();
      const match = output.match(/listening on (http:\/\/\S+)\r?\n/);
      if (match) resolve(match[1]);
    });
    server.once("exit", (code) => reject(new Error(`grafeno exited with ${code}: ${output}`)));
  });
});

after(async () => {
  server.kill();
  await once(server, "exit");
});

test("users sign in, refresh and sign out", async () => {
  const admin = new Grafeno({ url, token });
  assert.equal((await admin.authStatus()).open, false);
  const account = await admin.createAccount({
    name: "ana",
    kind: "user",
    password: "her long passphrase",
    grants: [{ namespace: "notes", role: "write" }],
  });
  assert.equal(account.has_password, true);

  const seen: (SessionTokens | null)[] = [];
  const client = new Grafeno({ url, onSession: (tokens) => seen.push(tokens) });
  await assert.rejects(client.signIn("ana", "not her passphrase"), (error) => error instanceof GrafenoError && error.status === 401);
  const tokens = await client.signIn("ana", "her long passphrase");
  assert.ok(tokens.access_token.startsWith("gfs_"));
  assert.equal(seen.length, 1);
  const me = await client.whoami();
  assert.equal(me.name, "ana");
  assert.equal(me.via?.kind, "session");
  await client.namespace("notes").createNode({ labels: ["Note"], props: { title: "hello" } });
  await assert.rejects(client.namespace("other").stats(), (error) => error instanceof GrafenoError && error.status === 403);

  // Refreshing replaces the tokens; the application gets them to keep.
  const renewed = await client.refreshSession();
  assert.notEqual(renewed.access_token, tokens.access_token);
  assert.equal(seen.length, 2);
  // A client built from kept tokens goes on with the session.
  const later = new Grafeno({ url, session: renewed });
  assert.equal((await later.namespace("notes").stats()).nodes, 1);
  const sessions = await later.sessions();
  assert.equal(sessions.length, 1);
  assert.equal(sessions[0].current, true);

  // A key of her own, read only.
  const { secret } = await later.createKey({ name: "ana-reader", scope: { read_only: true } });
  const reader = new Grafeno({ url, token: secret });
  assert.equal((await reader.namespace("notes").stats()).nodes, 1);
  await assert.rejects(reader.namespace("notes").createNode({ labels: ["Note"] }), (error) => error instanceof GrafenoError && error.status === 403);

  await later.signOut();
  assert.equal(seen.length, 2, "the first client was not told: it is another client");
  await assert.rejects(client.namespace("notes").stats(), (error) => error instanceof GrafenoError && error.status === 401);
  // The key outlives the session.
  assert.equal((await reader.namespace("notes").stats()).nodes, 1);
});

test("roles and grants (14.3)", async () => {
  const admin = new Grafeno({ url, token });
  const catalog = await admin.privilegeCatalog();
  assert.ok(catalog.server.includes("manage_roles"));
  await admin.putRole("team-reader", { description: "reads the teams" });
  const granted = await admin.grant(["read", "export"], { on: "namespace", namespace: "team-*" }, ["team-reader"]);
  assert.equal(granted.rows[0][0], "granted 2 privileges on namespace team-* to team-reader");
  await admin.grant(["read"], { on: "namespace", namespace: "team-secret" }, ["team-reader"], { deny: true });
  const listed = await admin.privilegesOf("team-reader");
  assert.equal(listed.rows.length, 3);
  const revoked = await admin.revoke(["export"], { on: "namespace", namespace: "team-*" }, ["team-reader"]);
  assert.equal(revoked.rows[0][0], "revoked 1 privilege on namespace team-* from team-reader");

  await admin.createAccount({ name: "bot", kind: "service" });
  await admin.grantRoles(["team-reader"], ["bot"]);
  const { secret } = await admin.createKey({ name: "bot-key", account: "bot" });
  const bot = new Grafeno({ url, token: secret });
  await bot.namespace("team-a").stats();
  await assert.rejects(bot.namespace("team-secret").stats(), (error) => error instanceof GrafenoError && error.status === 403);
  // A GQ access statement through the ordinary query call.
  const shown = await admin.namespace("default").query("SHOW PRIVILEGES FOR bot");
  assert.equal(shown.rows.length, 2);
  await assert.rejects(bot.namespace("team-a").query("CREATE ROLE x"), (error) => error instanceof GrafenoError && error.status === 403);

  const later = await admin.changeAccount("bot", { expires_at: "2020-01-01" });
  assert.ok(later.expires_at);
  await assert.rejects(bot.namespace("team-a").stats(), (error) => error instanceof GrafenoError && error.status === 401);
  await admin.changeAccount("bot", { expires_at: null });
  await admin.revokeRoles(["team-reader"], ["bot"]);
  await assert.rejects(bot.namespace("team-a").stats(), (error) => error instanceof GrafenoError && error.status === 403);
  assert.deepEqual((await admin.roles()).map((role) => role.name), ["team-reader"]);
  await admin.deleteRole("team-reader");
});
