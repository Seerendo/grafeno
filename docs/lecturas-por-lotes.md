# Lecturas por lotes de nodos y aristas

**Español** | [English](read-many.md)

`POST /v1/{namespace}/read-many` recibe listas opcionales `nodes` y `edges` con
IDs UUID. Admite hasta **512 posiciones combinadas**, contando duplicados. Una
sola transacción de lectura obtiene ambos grupos y los nodos incluyen los mismos
campos de texto y memoria que `GET /nodes/{id}`.

```ts
const records = await vault.readMany({
  nodes: [ana.id, bob.id, ana.id],
  edges: [relacion.id],
});
```

Cada lista de respuesta conserva el orden y los duplicados de su lista de
entrada. Un ID ausente en ese namespace produce `null`, sin interrumpir el resto.
Las listas omitidas son vacías. Los IDs inválidos, campos desconocidos y lotes
mayores de 512 posiciones producen HTTP 400. Se requiere permiso de lectura
sobre el namespace; los IDs de otro namespace no revelan registros. Las aristas
retractadas se devuelven igual que en la lectura individual por ID.

En Rust, `ReadTx::nodes_by_id(ns, ids)` y `ReadTx::edges_by_id(ns, ids)` conservan
esas posiciones. El límite de 512 pertenece a HTTP, no a esos métodos. Para que
nodos y aristas correspondan al mismo instante, utiliza el mismo `ReadTx`.

Esta operación no modifica registros ni registra accesos de memoria. Es distinta
de `/batch`, que agrupa escrituras. Las búsquedas por filtros y los recorridos de
vecinos conservan sus APIs existentes; `read-many` obtiene registros cuyos IDs
ya se conocen.

## Medición reproducible

El script inicia y termina su propio servidor en memoria y un puerto local
asignado por el sistema. No usa una base ni un servidor del usuario. Crea 256
nodos con texto y 256 aristas, compara lecturas individuales secuenciales,
lecturas con un máximo de ocho solicitudes concurrentes y una solicitud por lote.
Comprueba igualdad de resultados fuera del intervalo medido, calienta cada
estrategia tres veces y rota su orden durante 25 repeticiones.

```bash
cargo build --locked -p grafeno-server
node clients/typescript/bench/read-many.mjs
```

`GRAFENO_BIN` permite elegir el ejecutable y `GRAFENO_BENCH_ROUNDS` cambia las
repeticiones. El script emite JSON con medianas, p95 y entorno. En PowerShell,
utiliza `$env:GRAFENO_BIN` y `$env:GRAFENO_BENCH_ROUNDS` para esas opciones.

Medianas en milisegundos, medidas el 6 de noviembre de 2026 en Linux x64,
Intel i7-10750H, Node 24.22.2 y el perfil `dev` del repositorio:

| Nodos + aristas | Solicitudes individuales | Secuencial | Concurrencia 8 | Lote (1 solicitud) |
| --- | ---: | ---: | ---: | ---: |
| 1 + 1 | 2 | 0,976 | 0,735 | 0,606 |
| 16 + 16 | 32 | 8,544 | 4,770 | 0,590 |
| 64 + 64 | 128 | 28,755 | 15,319 | 1,020 |
| 256 + 256 | 512 | 114,492 | 55,404 | 3,179 |

[Datos completos, p95 y entorno](data/read-many-local.json).

El resultado mide el recorrido HTTP local, JSON y cliente, con los datos en
memoria. No mide Tauri, disco, caché fría, presión de escrituras ni tamaños de
texto mayores. Las lecturas individuales usan instantáneas independientes;
el benchmark no escribe mientras mide. El lote sí ofrece una instantánea común.
Los tiempos no son garantías ni umbrales de CI. Reducir solicitudes explica
parte de la mejora; no demuestra que el trabajo interno de Rust sea tantas
veces más rápido.

## Alcance y siguiente decisión

El motor recorre los IDs dentro de una instantánea; esta entrega no crea un
pool de hilos para cada lote. La concurrencia ocho es una estrategia de
comparación del benchmark. Primero conviene reducir cruces entre UI y motor;
el paralelismo interno necesita mediciones propias con carga, datos en disco y
límites de memoria antes de incorporarse.

Studio agrupa ahora los vecindarios de su expansión mediante otra operación,
`neighborhoods`. Esa carga de trabajo difiere de `read-many`; consulta la
[guía de expansión del grafo](expansion-grafo.md) para su contrato y medición.

## Pruebas

```bash
cargo test --locked -p grafeno --test read_many
cargo test --locked -p grafeno-server --test http read_many
cargo test --locked -p grafeno-server --test access
npm run build --prefix clients/typescript
npm test --prefix clients/typescript
```

Las pruebas cubren orden, duplicados, ausentes, aislamiento, límites y una
instantánea que conserva nodos y aristas tras una escritura posterior. La
prueba de ventana también llama a `read-many` por el puente Tauri.
