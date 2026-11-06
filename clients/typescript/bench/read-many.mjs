// Comparación reproducible sobre un servidor privado en memoria, sin datos del usuario.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { cpus } from 'node:os';
import { fileURLToPath } from 'node:url';
import { Grafeno } from '../src/index.ts';

const rounds = Number(process.env.GRAFENO_BENCH_ROUNDS ?? 25);
assert(Number.isInteger(rounds) && rounds >= 1 && rounds <= 1000, 'GRAFENO_BENCH_ROUNDS debe estar entre 1 y 1000');
const binary = process.env.GRAFENO_BIN ?? fileURLToPath(new URL(`../../../target/debug/grafeno${process.platform === 'win32' ? '.exe' : ''}`, import.meta.url));
const token = 'benchmark-privado';
const server = spawn(binary, ['serve', '--memory', '--addr', '127.0.0.1:0', '--token', token], { stdio: ['ignore', 'ignore', 'pipe'] });
const exited = once(server, 'exit');
// El fallo de arranque también se gestiona en la promesa de dirección.
exited.catch(() => {});
try {
  const url = await new Promise((resolve, reject) => {
    const timeout = setTimeout(() => reject(new Error('El servidor no inició en 10 s')), 10000);
    let output = '';
    const fail = error => { clearTimeout(timeout); reject(error); };
    server.once('error', fail);
    server.once('exit', code => fail(new Error(`El servidor terminó (${code}): ${output}`)));
    server.stderr.on('data', chunk => {
      output += chunk.toString();
      const match = output.match(/listening on (http:\/\/\S+)\r?\n/);
      if (match) { clearTimeout(timeout); resolve(match[1]); }
    });
  });
  const lab = new Grafeno({ url, token }).namespace('benchmark-lecturas');
  const created = await lab.batch(Array.from({ length: 256 }, (_, i) => ({ op: 'create_node', ref: `$n${i}`, node: { labels: ['Note'], props: { i }, text: 'Memoria de prueba: nodos, grafos y aristas. '.repeat(4) } })));
  const nodes = Array.from({ length: 256 }, (_, i) => created.refs[`$n${i}`]);
  const linked = await lab.batch(nodes.map((from, i) => ({ op: 'create_edge', edge: { from, to: nodes[(i + 1) % nodes.length], label: 'next' } })));
  const edges = linked.results.map(item => item.id);
  assert(edges.every(id => typeof id === 'string'));
  const results = [];
  for (const count of [1, 16, 64, 256]) {
    const input = { nodes: nodes.slice(0, count), edges: edges.slice(0, count) };
    const tasks = [...input.nodes.map(id => () => lab.getNode(id)), ...input.edges.map(id => () => lab.getEdge(id))];
    const individual = async concurrency => {
      let cursor = 0;
      const values = Array(tasks.length);
      await Promise.all(Array.from({ length: Math.min(concurrency, tasks.length) }, async () => {
        while (cursor < tasks.length) {
          const position = cursor++;
          values[position] = await tasks[position]();
        }
      }));
      return { nodes: values.slice(0, count), edges: values.slice(count) };
    };
    const methods = [
      { name: 'secuencial', requests: tasks.length, run: () => individual(1) },
      { name: 'paralelo_8', requests: tasks.length, run: () => individual(8) },
      { name: 'lote', requests: 1, run: () => lab.readMany(input) },
    ];
    const expected = await lab.readMany(input);
    const timings = methods.map(() => []);
    // Calentar todos los recorridos y rotar el orden para reducir el sesgo de caché.
    for (let round = -3; round < rounds; round++) {
      for (let offset = 0; offset < methods.length; offset++) {
        const index = (round + 3 + offset) % methods.length;
        const start = performance.now();
        const actual = await methods[index].run();
        const elapsed = performance.now() - start;
        assert.deepEqual(actual, expected, `${methods[index].name}: resultados distintos`);
        if (round >= 0) timings[index].push(elapsed);
      }
    }
    for (const [index, method] of methods.entries()) {
      const sorted = timings[index].sort((a, b) => a - b);
      results.push({ nodes: count, edges: count, method: method.name, requests: method.requests, median_ms: (sorted[Math.floor((sorted.length - 1) / 2)] + sorted[Math.floor(sorted.length / 2)]) / 2, p95_ms: sorted[Math.ceil(sorted.length * 0.95) - 1] });
    }
  }
  console.log(JSON.stringify({ date: new Date().toISOString(), node: process.version, platform: `${process.platform}/${process.arch}`, cpu: cpus()[0]?.model, logical_cpus: cpus().length, rounds, warmup: 3, environment: 'HTTP loopback; servidor en memoria; binario seleccionado por GRAFENO_BIN; sin Tauri ni disco', results }, null, 2));
} finally {
  if (server.pid && server.exitCode === null && server.signalCode === null) {
    server.kill();
    await exited;
  }
}
