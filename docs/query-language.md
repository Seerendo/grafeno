# GQ, the Grafeno query language

GQ is a subset of Cypher (and of ISO GQL's pattern style) plus Grafeno's own
features: facts with valid time and record time, retraction, and
pseudo-properties for what is not a property (ids, indexed text,
embeddings). It runs in `grafeno shell`, over HTTP (`POST /v1/{ns}/query`),
over MCP (the read-only `query` tool), and from Rust
(`grafeno_query::execute`).

```text
MATCH (a:Person {name: 'Ana'})-[w:works_at]->(c:Company)
VALID AT '2022-06-01'
WHERE c.name STARTS WITH 'Ac'
RETURN a.name, c.name, date(w.@valid_from) AS since
ORDER BY since DESC
LIMIT 10;
```

Every statement runs on one namespace. Keywords are case-insensitive;
labels, property names and variables are not. `//` starts a comment.

## Reading

### Patterns

| Pattern                          | Matches                                             |
| -------------------------------- | --------------------------------------------------- |
| `(n)`                            | any node                                            |
| `(n:Person:Admin)`               | nodes with all of these labels                      |
| `(n {name: 'Ana', age: $age})`   | nodes whose properties equal these values           |
| `(n {@id: '0190…'})`             | the node with this id (fast lookup)                 |
| `(a)-[r:knows]->(b)`             | an outgoing edge labeled `knows`                    |
| `(a)<-[:knows]-(b)`, `(a)-[]-(b)`| an incoming edge; an edge in either direction       |
| `(a)-->(b)`, `(a)<--(b)`, `(a)--(b)` | the same, without brackets                      |
| `(a)-[:knows\|likes]->(b)`        | an edge with any of these labels                    |
| `(a)-[:knows*1..3]->(b)`         | 1 to 3 hops (`*` alone: 1 to 32; `*2`: exactly 2)   |
| `(a)-[r {role: 'lead'}]->(b)`    | edges whose properties equal these values           |

- Patterns chain: `(a)-[:knows]->(b)-[:works_at]->(c)`. A `MATCH` can list
  several patterns separated by commas; they share variables, and
  unrelated ones combine every match of each.
- One edge is used at most once per pattern.
- A variable-length edge's variable holds the list of edges of the path.
  Variable-length matches are shortest paths: each end node is reached once,
  at its shortest distance (Cypher returns every path).
- Several `MATCH` clauses can follow each other; each extends the rows of
  the previous one.

### Time: `VALID AT` and `KNOWN AT`

By default a `MATCH` sees current knowledge: edges that were not
retracted, whatever their valid time. After the patterns:

- `VALID AT t` keeps only facts true at `t` (`@valid_from <= t <
  @valid_to`).
- `KNOWN AT t` shows what Grafeno knew at `t`, including facts retracted
  later.

`t` is an ISO 8601 string (`'2024-05-01'`, `'2024-05-01T10:30:00Z'`), a
time value such as `datetime()`, or microseconds since the Unix epoch.

```text
MATCH (:Person {name: 'Ana'})-[:works_at]->(c) VALID AT '2021-01-01' RETURN c.name;
MATCH (p)-[r]->(q) KNOWN AT '2026-11-01T12:00:00Z' RETURN p, r, q;
```

### `WHERE`

Comparisons `= <> != < <= > >=`, `AND`, `OR`, `XOR`, `NOT`,
`IS [NOT] NULL`, `IN [list]`, `STARTS WITH`, `ENDS WITH`, `CONTAINS`, and
label tests (`n:Admin`). Logic is three-valued as in SQL and Cypher: a
comparison with `null` is unknown, and `WHERE` keeps only rows where the
condition is true.

### `RETURN`

```text
RETURN [DISTINCT] expression [AS name], …  |  RETURN *
       [ORDER BY expression [ASC|DESC], …] [SKIP n] [LIMIT n]
```

- Column names are the `AS` names, or the expressions as written.
- Aggregates: `count(*)`, `count(x)`, `sum`, `avg`, `min`, `max`,
  `collect`, each also with `DISTINCT` (`count(DISTINCT x)`). Items that
  are not aggregates are the grouping keys. Aggregates over no rows give
  one row (`count(*)` = 0) unless there are grouping keys.
- `ORDER BY` can use return names, variables, or expressions; an aggregate
  in `ORDER BY` must also be returned.
- `null` sorts last; values of different types sort by type.

## Writing

A statement with any write runs in one transaction: all of its changes or
none. Writes come after the `MATCH` clauses (a `MATCH` after a write is not
supported). Without `RETURN`, a writing statement reports only what
changed.

| Clause                                   | Effect                                                    |
| ---------------------------------------- | --------------------------------------------------------- |
| `CREATE (n:Note {title: 'x', @text: '…'})` | creates nodes (one label at least) and edges (one label, a direction) |
| `CREATE (a)-[:knows {@valid_from: '2024-01-01'}]->(b)` | creates an edge between bound or new nodes |
| `SET n.key = value`                      | sets a property (`null` removes it)                       |
| `SET n += {a: 1, b: null}`               | merges properties                                         |
| `SET n = {a: 1}`                         | replaces all properties                                   |
| `SET n:Label:Other`                      | adds labels                                               |
| `SET n.@text = '…'`, `SET n.@embedding = […]` | indexes text or a vector (`null` removes it)         |
| `SET n.@importance = 0.9`                | sets the importance, from 0 to 1                         |
| `SET r.@valid_to = '2025-01-01'`         | the fact stopped being true then (history is kept)        |
| `REMOVE n.key, n:Label, n.@text`         | removes properties, labels, the text or the embedding    |
| `DELETE n` (`DETACH DELETE n`)           | deletes nodes with their edges, or edges                  |
| `RETRACT r`                              | the fact was a mistake: hidden from current knowledge, visible to `KNOWN AT` earlier |

`CREATE` accepts `@text`, `@embedding` and `@sources` (a list of ids) for
nodes, and `@valid_from`, `@valid_to` and `@sources` for edges. After a
write, `RETURN` sees the written values.

## Pseudo-properties

Read with `n.@name`. They are what a node or edge has besides properties.

| Nodes          | Edges          |
| -------------- | -------------- |
| `@id`          | `@id`          |
| `@labels`      | `@label`       |
| `@text`        | `@from`, `@to` (node ids) |
| `@embedding`   | `@valid_from`, `@valid_to` |
| `@sources`     | `@recorded_at`, `@expired_at` |
| `@created_at`, `@updated_at` | `@sources` |
| `@importance` (0 to 1), `@accesses`, `@last_access` | |

Times are time values; they compare with ISO strings and print as ISO 8601.
`SET n.@importance = 0.9` changes a node's importance, which weighs in
recall ranking and decay (see the memory section of
[DESIGN.md](../DESIGN.md)).

## Expressions and functions

Literals: `1`, `2.5`, `1e3`, `'text'` or `"text"` (with `\n`, `\t`,
`\uXXXX`), `true`, `false`, `null`, lists `[1, 2]`, maps `{a: 1}`.
Parameters: `$name`. Operators: `+ - * / %` (integer division for
integers; `+` also joins strings and lists), `list[i]` (negative from the
end), `map.key`.

| Function | |
| --- | --- |
| `id(x)`, `labels(n)`, `type(r)` / `label(r)` | Cypher-style access |
| `properties(x)`, `keys(x)`, `text(n)`, `startNode(r)`, `endNode(r)` | |
| `size(x)` / `length(x)`, `head(list)`, `last(list)`, `range(a, b[, step])` | |
| `lower`, `upper`, `trim`, `substring(s, start[, length])`, `replace(s, a, b)`, `split(s, sep)` | |
| `coalesce(…)`, `toString`, `toInteger`, `toFloat` | |
| `abs`, `round`, `floor`, `ceil`, `sqrt` | |
| `datetime([x])` / `time([x])`, `date(t)` (`YYYY-MM-DD`), `now()` | |

## Not supported (yet)

`OPTIONAL MATCH`, `MERGE`, `WITH`, `UNWIND`, `CALL`, `UNION`, `FOREACH`,
`CASE`, list comprehensions, pattern predicates in `WHERE`, and indexes on
property values (a `MATCH` by property scans the nodes of its label, or of
the namespace without a label). For full-text, vector and hybrid search,
use the shell's `\search`, `\recall` and `\context`, or the HTTP API.
