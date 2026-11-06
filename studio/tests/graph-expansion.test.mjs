import assert from 'node:assert/strict';
import { test } from 'node:test';
import { GrafenoError } from '../../clients/typescript/src/index.ts';
import { loadExpandedGraph } from '../src/graph/expansion.ts';

const node = id => ({ id, labels: ['Note'], props: {}, sources: [], created_at: 0, updated_at: 0 });
const neighborhood = id => ({ nodes: [node(id)], edges: [] });
const signal = () => new AbortController().signal;

test('agrupa 18 raíces en 8, 8 y 2 sin cambiar su orden ni el filtro', async () => {
  const calls = [];
  const roots = Array.from({ length: 18 }, (_, i) => `n${i}`);
  const filter = { valid_at: '2026-01-01' };
  const result = await loadExpandedGraph({
    async neighborhoods(seeds, options) { calls.push(seeds); assert.deepEqual(options, { filter }); return seeds.map(neighborhood); },
    async subgraph() { assert.fail('no debe pedir raíces individuales'); },
  }, roots, filter, signal());
  assert.deepEqual(calls.map(list => list.length), [8, 8, 2]);
  assert.deepEqual([...result.nodes.keys()], roots);
});

test('conserva el límite visual por raíz y comparte nodos y aristas sin duplicarlos', async () => {
  const numbered = Array.from({ length: 300 }, (_, i) => node(`n${i}`));
  const edge = { id: 'edge', from: 'a', to: 'b' };
  const result = await loadExpandedGraph({
    async neighborhoods() { return [{ nodes: [node('a'), ...numbered.slice(0, 200)], edges: [edge] }, { nodes: [node('b'), ...numbered], edges: [edge] }]; },
  }, ['a', 'b'], undefined, signal());
  assert.equal(result.nodes.size, 300);
  assert(result.nodes.has('a') && result.nodes.has('b') && result.nodes.has('n297'));
  assert(!result.nodes.has('n298'));
  assert.equal(result.edges.size, 1);
});

test('un servidor anterior usa hasta cuatro llamadas simultáneas y conserva el orden', async () => {
  let probes = 0, active = 0, peak = 0, calls = 0;
  const roots = Array.from({ length: 18 }, (_, i) => `n${i}`);
  const filter = { valid_at: '2030-01-01' };
  const result = await loadExpandedGraph({
    async neighborhoods() { probes++; throw new GrafenoError(404, 'http', 'Not Found'); },
    async subgraph(seeds, options) {
      calls++; active++; peak = Math.max(peak, active);
      assert.deepEqual(options, { depth: 1, filter });
      await new Promise(resolve => setImmediate(resolve));
      if (seeds[0] === 'n0') await new Promise(resolve => setImmediate(resolve));
      active--;
      return neighborhood(seeds[0]);
    },
  }, roots, filter, signal());
  assert.equal(probes, 1);
  assert.equal(calls, 18);
  assert.equal(peak, 4);
  assert.deepEqual([...result.nodes.keys()], roots);
});

for (const [status, code] of [[401, 'unauthorized'], [403, 'forbidden'], [404, 'not_found'], [500, 'http']]) {
  test(`el error ${status}/${code} se muestra sin activar compatibilidad`, async () => {
    const expected = new GrafenoError(status, code, 'fallo');
    await assert.rejects(loadExpandedGraph({
      async neighborhoods() { throw expected; },
      async subgraph() { assert.fail('no debe ocultar el error'); },
    }, ['a'], undefined, signal()), error => error === expected);
  });
}

test('una respuesta antigua se descarta y no solicita el siguiente lote', async () => {
  const controller = new AbortController();
  let finish, calls = 0;
  const pending = loadExpandedGraph({
    neighborhoods(seeds) { calls++; return new Promise(resolve => { finish = () => resolve(seeds.map(neighborhood)); }); },
  }, Array.from({ length: 18 }, (_, i) => `n${i}`), undefined, controller.signal);
  controller.abort();
  finish();
  await assert.rejects(pending, { name: 'AbortError' });
  assert.equal(calls, 1);
});

test('limpiar las raíces no pide datos y una señal cancelada no inicia llamadas', async () => {
  const graph = { async neighborhoods() { assert.fail('sin llamadas'); } };
  assert.equal((await loadExpandedGraph(graph, [], undefined, signal())).nodes.size, 0);
  const controller = new AbortController(); controller.abort();
  await assert.rejects(loadExpandedGraph(graph, ['a'], undefined, controller.signal), { name: 'AbortError' });
});

test('rechaza una respuesta incompleta en vez de asignarla a raíces equivocadas', async () => {
  await assert.rejects(loadExpandedGraph({ async neighborhoods() { return []; } }, ['a'], undefined, signal()), /response count/);
});

test('un 405 de endpoint no disponible también conserva la expansión individual', async () => {
  const result = await loadExpandedGraph({
    async neighborhoods() { throw new GrafenoError(405, 'http', 'Method Not Allowed'); },
    async subgraph([id]) { return neighborhood(id); },
  }, ['a', 'b'], undefined, signal());
  assert.deepEqual([...result.nodes.keys()], ['a', 'b']);
});

test('un fallo de compatibilidad detiene las raíces aún no enviadas', async () => {
  let calls = 0;
  await assert.rejects(loadExpandedGraph({
    async neighborhoods() { throw new GrafenoError(404, 'http', 'Not Found'); },
    async subgraph([id]) {
      calls++;
      if (id === 'n0') throw new GrafenoError(403, 'forbidden', 'Sin permiso');
      await new Promise(resolve => setImmediate(resolve));
      return neighborhood(id);
    },
  }, Array.from({ length: 8 }, (_, i) => `n${i}`), undefined, signal()), error => error.status === 403);
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(calls, 4);
});
