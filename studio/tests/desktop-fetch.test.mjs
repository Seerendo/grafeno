import assert from 'node:assert/strict';
import { test } from 'node:test';
import { createDesktopFetch } from '../src/desktop-fetch.ts';

const json = (value, status = 200) => ({ status, headers: { 'content-type': 'application/json' }, body: JSON.stringify(value) });

test('conserva método, cabeceras y cuerpo de un Request con Unicode', async () => {
  const fetch = createDesktopFetch(async (command, args) => {
    assert.equal(command, 'request');
    assert.equal(args.method, 'PATCH');
    assert.equal(args.url, 'http://grafeno.local/session/lab/v1/query');
    assert.equal(args.headers.authorization, 'Bearer secreto');
    assert.equal(Buffer.from(args.bodyBase64, 'base64').toString(), 'Memoria de Ana: café y grafos');
    return json({ ok: true });
  });
  const response = await fetch(new Request('http://grafeno.local/session/lab/v1/query', {
    method: 'PATCH', headers: { authorization: 'Bearer secreto' }, body: 'Memoria de Ana: café y grafos',
  }));
  assert.deepEqual(await response.json(), { ok: true });
});

test('RequestInit sustituye las opciones del Request original', async () => {
  const fetch = createDesktopFetch(async (_, args) => {
    assert.equal(args.method, 'PUT');
    assert.equal(args.headers['x-test'], 'nuevo');
    assert.equal(Buffer.from(args.bodyBase64, 'base64').toString(), 'nuevo');
    return json({});
  });
  await fetch(new Request('http://example.test', { method: 'POST', headers: { 'x-test': 'anterior' }, body: 'anterior' }),
              { method: 'PUT', headers: { 'x-test': 'nuevo' }, body: 'nuevo' });
});

test('conserva todos los valores de byte en solicitudes y respuestas binarias', async () => {
  const bytes = Uint8Array.from({ length: 100_003 }, (_, i) => i % 256);
  const fetch = createDesktopFetch(async (_, args) => {
    assert.deepEqual(Buffer.from(args.bodyBase64, 'base64'), Buffer.from(bytes));
    return { status: 200, headers: { 'content-type': 'application/octet-stream' }, body: '', bodyBase64: args.bodyBase64 };
  });
  const response = await fetch('http://example.test/backup', { method: 'POST', body: bytes });
  assert.deepEqual(new Uint8Array(await response.arrayBuffer()), bytes);
});

for (const status of [204, 205, 304]) {
  test(`la respuesta ${status} no tiene cuerpo`, async () => {
    const response = await createDesktopFetch(async () => json({}, status))('http://example.test');
    assert.equal(response.body, null);
  });
}

test('HEAD conserva el estado y las cabeceras sin un cuerpo', async () => {
  const response = await createDesktopFetch(async () => json({}, 200))('http://example.test', { method: 'HEAD' });
  assert.equal(response.body, null);
  assert.equal(response.headers.get('content-type'), 'application/json');
});

test('un error HTTP se devuelve como respuesta y no provoca un reintento', async () => {
  let calls = 0;
  const response = await createDesktopFetch(async () => { calls++; return json({ error: 'missing' }, 404); })('http://example.test');
  assert.equal(response.status, 404);
  assert.deepEqual(await response.json(), { error: 'missing' });
  assert.equal(calls, 1);
});

test('un fallo del comando nativo se devuelve como TypeError sin reintentos', async () => {
  let calls = 0;
  await assert.rejects(createDesktopFetch(async () => { calls++; throw 'sin conexión'; })('http://example.test'), TypeError);
  assert.equal(calls, 1);
});

test('una solicitud cancelada antes de salir no invoca el motor', async () => {
  const controller = new AbortController();
  controller.abort();
  let calls = 0;
  await assert.rejects(createDesktopFetch(async () => { calls++; return json({}); })('http://example.test', { signal: controller.signal }), { name: 'AbortError' });
  assert.equal(calls, 0);
});

test('cancelar deja de esperar sin repetir la escritura ni prometer revertirla', async () => {
  const controller = new AbortController();
  let finish;
  let started;
  const dispatched = new Promise(resolve => { started = resolve; });
  let calls = 0;
  const fetch = createDesktopFetch(() => {
    calls++;
    started();
    return new Promise(resolve => { finish = resolve; });
  });
  const pending = fetch('http://example.test', { method: 'POST', body: 'una escritura', signal: controller.signal });
  await dispatched;
  controller.abort();
  await assert.rejects(pending, { name: 'AbortError' });
  finish(json({ committed: true }));
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(calls, 1);
});


test('cancelar durante la lectura del cuerpo evita enviar la orden', async () => {
  const controller = new AbortController();
  let stream;
  const body = new ReadableStream({ start(value) { stream = value; } });
  let calls = 0;
  const fetch = createDesktopFetch(async () => { calls++; return json({}); });
  const pending = fetch('http://example.test', { method: 'POST', body, duplex: 'half', signal: controller.signal });
  await new Promise(resolve => setImmediate(resolve));
  controller.abort();
  await assert.rejects(pending, { name: 'AbortError' });
  stream.close();
  assert.equal(calls, 0);
});
