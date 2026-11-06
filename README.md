# Grafeno

<p align="center">
  <img src="docs/assets/readme-banner-es.svg" width="1440" alt="Símbolo de Grafeno: tres anillos de carbono conectados a una red de nodos.">
</p>

<p align="center">
  <strong>Español</strong> · <a href="README.en.md">English</a><br>
  <a href="#empezar-por-el-escritorio">Empezar</a> ·
  <a href="docs/install.md">Instalación</a> ·
  <a href="docs/query-language.md">Consultas GQ</a> ·
  <a href="CONTRIBUTING.md">Contribuir</a>
</p>

Grafeno es un **motor de grafos en Rust para memoria y conocimiento de agentes**.
Conecta entidades, documentos y hechos; conserva su procedencia e historial;
y combina búsqueda por texto, vectores y relaciones para recuperar contexto.
Su primer cliente es [Grafito](https://github.com/Seerendo/grafito).

En el escritorio, abre una base local con el motor dentro de la aplicación
Tauri. También puede utilizarse como biblioteca Rust, servidor HTTP o servidor
MCP para agentes.

## Qué puedes hacer

| Capacidad | Para qué sirve |
| --- | --- |
| **Conectar conocimiento** | Nodos con etiquetas y propiedades, relaciones dirigidas y namespaces para organizar los datos. |
| **Consultar el pasado** | Distinguir cuándo un hecho era válido y cuándo el sistema lo conoció mediante `VALID AT` y `KNOWN AT`. |
| **Encontrar contexto** | Combinar búsqueda BM25, vectores HNSW y expansión del grafo; preparar contexto con un presupuesto de tokens. |
| **Mantener la memoria** | Resolver entidades, revisar posibles duplicados, registrar fusiones reversibles y resumir comunidades sin un modelo. |
| **Explorar y consultar** | Visualizar relaciones, editar información y ejecutar GQ, un lenguaje parecido a Cypher. |
| **Operar la base** | Usar claves con permisos, respaldos, importación y exportación, comprobaciones de integridad y reconstrucción de índices. |

## Empezar por el escritorio

**Grafeno Desktop** permite crear o abrir archivos `.redb`, recuperar bases
recientes y conectarse a un servidor. El modo local ejecuta el motor dentro del
proceso: no necesita un servicio ni abre un puerto.

Para conocer los paquetes de Windows y Linux, consulta la
[guía de instalación](docs/install.md) y la
[documentación de escritorio](apps/desktop/README.md).

Para ejecutarlo desde el código, prepara primero los
[requisitos de desarrollo](CONTRIBUTING.md#requisitos). Desde la raíz:

```bash
npm ci --prefix studio
npm run build --prefix studio
cargo run --locked -p grafeno-desktop
```

Un archivo `.redb` solo puede estar abierto en un proceso a la vez. Cierra la
base en el escritorio antes de abrir ese mismo archivo con el servidor o la
consola, y viceversa.

## Una consulta, relaciones e historia

Con el binario `grafeno` instalado, abre una consola local:

```bash
grafeno shell --db ejemplo.redb -n demo
```

Crea una persona y una relación con fecha de inicio:

```cypher
CREATE (a:Person {name: 'Ana'})
  -[:works_at {@valid_from: '2024-01-01'}]->
  (h:Organization {name: 'Hospital Central'})
RETURN a.name, h.name;
```

Consulta qué era válido en una fecha:

```cypher
MATCH (p:Person)-[r:works_at]->(o:Organization)
VALID AT '2024-06-01'
RETURN p.name, o.name, r.@valid_from;
```

GQ también incluye agregaciones, índices y algoritmos de caminos, centralidad,
comunidades y similitud. Consulta la [referencia de GQ](docs/query-language.md).

## Integrar aplicaciones y agentes

| Forma de acceso | Punto de entrada |
| --- | --- |
| Escritorio local o conexión a un servidor | [Aplicación Tauri](apps/desktop/README.md) |
| Servicio HTTP, servidor MCP y consola | [Servidor Grafeno](crates/grafeno-server/README.md) |
| Aplicaciones JavaScript y TypeScript | [Cliente TypeScript](clients/typescript/README.md) |
| Biblioteca Rust embebida | [`grafeno`](crates/grafeno/src/lib.rs) y [`grafeno-memory`](crates/grafeno-memory/src/lib.rs) |

Para iniciar MCP con una base propia, después de instalar el binario:

```bash
grafeno mcp --db agente.redb
```

Para probar el servidor HTTP con datos temporales en memoria:

```bash
grafeno serve --memory
```

Escucha por defecto en `127.0.0.1:7810`. Studio está en
<http://127.0.0.1:7810/studio/> cuando su compilación se incluye en el binario.
Los datos de `--memory` se pierden al cerrar el proceso. Para una configuración
persistente y autenticada, sigue la [guía del servidor](docs/install.md#settings).

## Cómo está organizado

| Directorio | Responsabilidad |
| --- | --- |
| `crates/grafeno-core` | Modelo de nodos, relaciones, identificadores y tiempo. |
| `crates/grafeno` | Almacenamiento redb, transacciones, índices, recorridos y algoritmos. |
| `crates/grafeno-memory` | Recuperación híbrida, contexto, extracción y mantenimiento de memoria. |
| `crates/grafeno-query` | Parser, planificación y ejecución de GQ. |
| `crates/grafeno-server` | API HTTP, MCP, consola y administración. |
| `studio` | Interfaz compartida por el navegador y el escritorio. |
| `apps/desktop` | Ventana Tauri, archivos locales, menús y puente hacia el motor. |
| `clients/typescript` | Cliente HTTP para aplicaciones. |

## Desarrollo y pruebas

La [guía de contribución en español](CONTRIBUTING.md) y su
[versión en inglés](CONTRIBUTING.en.md) incluyen los requisitos, la compilación
del escritorio y una alternativa que reduce el espacio temporal utilizado.

Para verificar el motor sin instalar las bibliotecas gráficas de Tauri:

```bash
cargo test --workspace --locked --exclude grafeno-desktop
```

El [workflow de contribuciones](.github/workflows/ci.yml) verifica interfaz y
cliente TypeScript, motor y servidor, integración del cliente, pruebas del
puente Tauri, compilación y flujos de la ventana en Linux con capturas.
La revisión visual manual y los instaladores requieren comprobaciones adicionales.

## Diseño, rendimiento y alcance

- [Diseño y decisiones](DESIGN.md): arquitectura, almacenamiento y semántica temporal.
- [Rendimiento](docs/performance.md) y [benchmarks](docs/benchmarks.md): métodos, datos y límites de las mediciones del proyecto.
- [Lecturas por lotes: contrato, pruebas y medición reproducible](docs/lecturas-por-lotes.md).
- [Historial de desarrollo](docs/development-history.md): detalle de las fases documentadas originalmente.

Los embeddings los proporciona el cliente. El presupuesto de contexto utiliza
una estimación de tokens por defecto; la biblioteca Rust permite proporcionar
el contador del modelo. El motor admite múltiples lectores y un escritor;
no implementa distribución ni sharding. La licencia sigue como decisión abierta
en el diseño.

Creado por [Seerendo](https://github.com/Seerendo). La identidad gráfica representa
tres anillos de carbono fusionados de una red de grafeno.
