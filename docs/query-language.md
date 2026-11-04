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
- `OPTIONAL MATCH` keeps each row even when its patterns do not match: the
  new variables are then `null`. Its `WHERE` filters the optional part
  only.

```text
MATCH (p:Person)
OPTIONAL MATCH (p)-[:works_at]->(c)
RETURN p.name, c.name;     // people without a company get null
```

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

**Pattern predicates** test whether a pattern matches from the row, with
the `MATCH`'s `VALID AT` and `KNOWN AT`:

```text
MATCH (p:Person) WHERE (p)-[:knows]->()            RETURN p;  // knows someone
MATCH (p:Person) WHERE NOT (p)<-[:knows]-()        RETURN p;  // nobody knows p
MATCH (p:Person) WHERE exists((p)-[:works_at]->()) RETURN p;
MATCH (p:Person)
WHERE EXISTS { MATCH (p)-[:knows]->(f) WHERE f.age > 40 }
RETURN p;
```

`exists(n.key)` is true when the property is set.

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

### `WITH`: steps between clauses

`WITH` is a `RETURN` in the middle of a query: it projects, groups and
sorts the rows, and only its names stay in scope. A `WHERE` after it
filters the projected rows, so it can filter aggregates (SQL's `HAVING`).
Expressions other than variables need a name (`AS`). `WITH *` keeps every
variable.

```text
// Companies with more than one person
MATCH (p:Person)-[:works_at]->(c)
WITH c, count(p) AS people
WHERE people > 1
RETURN c.name, people ORDER BY people DESC;

// The two oldest people, then their companies
MATCH (p:Person) WITH p ORDER BY p.age DESC LIMIT 2
MATCH (p)-[:works_at]->(c) RETURN p.name, c.name;

// An aggregate of aggregates
MATCH (p:Person) OPTIONAL MATCH (p)-[:knows]->(f)
WITH p, count(f) AS friends
RETURN avg(friends), max(friends);
```

### `UNWIND`

One row per item of a list (`null` gives no rows):

```text
UNWIND [1, 2, 3] AS x RETURN x * 10;
MATCH (p:Person) UNWIND p.langs AS lang RETURN lang, count(*) AS people;
```

### `UNION`

Joins the rows of several queries with the same column names. `UNION`
drops duplicate rows; `UNION ALL` keeps them (one statement uses one or
the other).

```text
MATCH (p:Person) RETURN p.name AS name
UNION
MATCH (c:Company) RETURN c.name AS name;
```

### `CALL`: procedures

```text
CALL db.labels();                                     // label, count
CALL db.edgeLabels() YIELD label, count AS edges WHERE edges > 10 RETURN label;
CALL db.search('borrow checker', 5) YIELD node, score RETURN node.title, score;
```

| Procedure | Yields | |
| --- | --- | --- |
| `db.labels()` | `label`, `count` | node labels and their counts |
| `db.edgeLabels()` (`db.relationshipTypes()`) | `label`, `count` | edge labels and their counts |
| `db.propertyKeys()` | `key` | property names of nodes and edges (reads every record) |
| `db.stats()` | `nodes`, `edges`, `texts`, `vectors`, `dimension` | counts of the namespace |
| `db.search(text, limit = 10)` | `node`, `score` | full-text search (BM25), best first |
| `db.searchVector(embedding, limit = 10)` | `node`, `score` | nearest nodes by embedding |
| `dbms.procedures()` | `name`, `arguments`, `columns`, `description` | this list |

`YIELD` picks and renames columns (`YIELD label AS l`); without it, every
column. Arguments can use the row's variables, so a `CALL` in the middle of
a query runs once per row. A query may end with `CALL`: it returns what the
procedure yields.

### `EXPLAIN` and `PROFILE`

`EXPLAIN` before a statement returns its plan as a table (step, operation,
details: where each pattern starts and how it expands) without running it.
`PROFILE` runs the statement and adds, per step, the rows it produced and
the milliseconds it took (`profile` in the JSON result, a second table in
the shell, the *Profile* view in the Studio).

```text
EXPLAIN MATCH (a:Person {name: 'Ana'})-[:knows]->(f) RETURN f.name;
PROFILE MATCH (p:Person) WHERE p.age > 30 RETURN count(p);
```

## Writing

A statement with any write runs in one transaction: all of its changes or
none. Clauses after a write see what it wrote (a `MATCH` after a `CREATE`
finds the new nodes). Without `RETURN`, a writing statement reports only
what changed.

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
| `MERGE (n:Tag {name: 'rust'})`           | matches the pattern, or creates it when nothing matches  |
| `FOREACH (x IN list \| CREATE …)`         | runs writes once per item                                 |

`CREATE` accepts `@text`, `@embedding` and `@sources` (a list of ids) for
nodes, and `@valid_from`, `@valid_to` and `@sources` for edges. After a
write, `RETURN` sees the written values.

### `MERGE`

`MERGE pattern` matches the whole pattern from the row; when nothing
matches, it creates the parts that are not bound. `ON CREATE SET` and
`ON MATCH SET` run in each case. Rows of the same statement see what
earlier rows merged, so `UNWIND ['a', 'a'] AS x MERGE (:Tag {name: x})`
creates one node. Only facts true now count as existing: if a `knows` edge
was ended, `MERGE (a)-[:knows]->(b)` records the fact again. An undirected
edge in `MERGE` is created from left to right.

```text
MERGE (t:Tag {name: $name})
ON CREATE SET t.created = datetime()
ON MATCH SET t.uses = coalesce(t.uses, 0) + 1
RETURN t;

MATCH (a:Person {name: 'Ana'}), (b:Person {name: 'Bob'})
MERGE (a)-[:knows]->(b);
```

### `FOREACH`

Runs write clauses (`CREATE`, `MERGE`, `SET`, `REMOVE`, `DELETE`,
`RETRACT`, `FOREACH`) once per item; what it binds stays inside.

```text
MATCH (a:Person {name: 'Ana'})
FOREACH (topic IN ['graphs', 'rust'] |
  MERGE (t:Topic {name: topic})
  CREATE (a)-[:likes]->(t));
```

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
integers; `+` also joins strings and lists), `^` (power, a float),
`list[i]` (negative from the end), `list[from..to]` (either end may be
left out), `map.key`.

```text
RETURN CASE WHEN p.age < 30 THEN 'young' ELSE 'experienced' END;  // searched
RETURN CASE p.status WHEN 'a' THEN 1 WHEN 'b' THEN 2 ELSE 0 END;  // simple
RETURN [x IN range(1, 10) WHERE x % 2 = 0 | x * x];               // comprehension
RETURN all(x IN p.scores WHERE x > 50), any(…), none(…), single(…);
RETURN reduce(total = 0, x IN [1, 2, 3] | total + x);             // 6
```

`CASE` without a matching `WHEN` and no `ELSE` gives `null`. Quantifiers
are three-valued: unknown items make the answer unknown unless the known
ones decide it.

| Function | |
| --- | --- |
| `id(x)`, `labels(n)`, `type(r)` / `label(r)` | Cypher-style access |
| `properties(x)`, `keys(x)`, `text(n)`, `startNode(r)`, `endNode(r)` | |
| `size(x)` / `length(x)`, `head(list)`, `last(list)`, `range(a, b[, step])` | |
| `lower`, `upper`, `trim`, `substring(s, start[, length])`, `replace(s, a, b)`, `split(s, sep)` | |
| `coalesce(…)`, `toString`, `toInteger`, `toFloat` | |
| `abs`, `round`, `floor`, `ceil`, `sqrt` | |
| `datetime([x])` / `time([x])`, `date(t)` (`YYYY-MM-DD`), `now()` | |
| `exists(pattern)`, `exists(n.key)` | pattern predicate; whether a property is set |

## Not supported (yet)

- `CALL { … }` subqueries, pattern comprehensions (`[(a)-->(b) | b.name]`),
  map projections (`n {.name}`) and `=~` regular expressions.
- Indexes on property values: a `MATCH` by property scans the nodes of its
  label, or of the namespace without a label (phase 8.3).
- Statistical aggregates and durations (phase 8.2), and graph algorithms as
  procedures (phase 8.5). Hybrid recall is in the shell's `\recall` and
  `\context`, and in the HTTP API.
