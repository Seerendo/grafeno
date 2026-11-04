import { useCallback, useEffect, useMemo, useRef, useState } from "react";

import { type QueryResult, describeError, go, labelColor } from "../api.ts";
import { type CodeEditorHandle, CodeEditor } from "../components/CodeEditor.tsx";
import { MiniGraph, hasGraph } from "../components/MiniGraph.tsx";
import { ResultTable } from "../components/ResultTable.tsx";
import { useStudio } from "../context.ts";
import { PSEUDO_PROPERTIES, type Statement, errorPosition, splitStatements, statementAt } from "../gq.ts";
import { Icon } from "../icons.tsx";

interface Script {
  id: string;
  name: string;
  text: string;
}

interface Library {
  scripts: Script[];
  active: string;
}

interface Run {
  statement: Statement;
  status: "ok" | "error";
  result?: QueryResult;
  error?: string;
  /** Where the error is in the script (1-based). */
  at?: { line: number; column: number };
  ms: number;
}

const STORAGE_KEY = "grafeno.studio.scripts";
const LAYOUT_KEY = "grafeno.studio.scripts.split";

const WELCOME = `// Grafeno script: GQ statements, each ending with ;
// Ctrl+Enter runs the statement under the cursor (or the selection),
// F5 or Ctrl+Shift+Enter runs the whole script. Each statement is its own transaction.

CREATE (ana:Person {name: 'Ana', @text: 'Ana writes Rust and likes tea'})
  -[:works_at {@valid_from: '2024-01-01'}]->(acme:Company {name: 'Acme'})
RETURN ana, acme;

MATCH (p:Person)-[w:works_at]->(c:Company)
RETURN p.name AS person, c.name AS company, w.@valid_from AS since;

MATCH (n)
RETURN labels(n) AS labels, count(*) AS nodes
ORDER BY nodes DESC;
`;

const SNIPPETS: { name: string; text: string }[] = [
  { name: "All nodes", text: "MATCH (n)\nRETURN n\nLIMIT 100;" },
  { name: "Nodes per label", text: "MATCH (n)\nRETURN labels(n) AS labels, count(*) AS nodes\nORDER BY nodes DESC;" },
  { name: "All facts", text: "MATCH (a)-[r]->(b)\nRETURN a, type(r) AS fact, b, r.@valid_from, r.@valid_to\nLIMIT 100;" },
  { name: "As of a date", text: "MATCH (a)-[r]->(b)\nVALID AT '2024-06-01'\nRETURN a, type(r), b;" },
  { name: "Two hops", text: "MATCH (a {name: 'Ana'})-[*1..2]-(b)\nRETURN DISTINCT b;" },
  { name: "Create a node", text: "CREATE (n:Note {title: 'Hello', @text: 'Searchable text'})\nRETURN n;" },
  { name: "Connect two nodes", text: "MATCH (a {name: 'Ana'}), (b {name: 'Acme'})\nCREATE (a)-[:knows {@valid_from: '2024-01-01'}]->(b);" },
  { name: "Change properties", text: "MATCH (n {name: 'Ana'})\nSET n.city = 'Lima', n.@importance = 0.9\nRETURN n;" },
  { name: "Important memories", text: "MATCH (n)\nWHERE n.@importance >= 0.7\nRETURN n, n.@importance AS importance, n.@accesses AS accesses\nORDER BY importance DESC;" },
  { name: "Delete", text: "MATCH (n:Note {title: 'Hello'})\nDETACH DELETE n;" },
];

function newId(): string {
  return Math.random().toString(36).slice(2, 10);
}

function loadLibrary(): Library {
  try {
    const saved = JSON.parse(localStorage.getItem(STORAGE_KEY) ?? "null") as Library | null;
    if (saved?.scripts?.length) return saved;
  } catch {
    // A broken library starts over.
  }
  const id = newId();
  return { scripts: [{ id, name: "Welcome", text: WELCOME }], active: id };
}

function saveLibrary(library: Library): void {
  try {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(library));
  } catch {
    // Scripts then last only for this page.
  }
}

function changes(stats: QueryResult["stats"]): string {
  return Object.entries(stats)
    .filter(([, count]) => count > 0)
    .map(([key, count]) => `${count} ${key.replaceAll("_", " ")}`)
    .join(", ");
}

function lineOf(text: string, offset: number): number {
  return text.slice(0, offset).split("\n").length;
}

function csvCell(value: unknown): string {
  let text: string;
  if (value === null || value === undefined) text = "";
  else if (typeof value === "object" && "id" in (value as object)) text = String((value as { id: string }).id);
  else if (typeof value === "object") text = JSON.stringify(value);
  else text = String(value);
  return /[",\n]/.test(text) ? `"${text.replaceAll('"', '""')}"` : text;
}

function download(name: string, text: string, type: string): void {
  const url = URL.createObjectURL(new Blob([text], { type }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}

export function ScriptsView() {
  const { client, namespace, schema, changed, confirm, toast } = useStudio();
  const [library, setLibrary] = useState(loadLibrary);
  const [runs, setRuns] = useState<Run[]>([]);
  const [running, setRunning] = useState(false);
  const [stopOnError, setStopOnError] = useState(true);
  const [tab, setTab] = useState<"messages" | number>("messages");
  const [mode, setMode] = useState<"table" | "json" | "graph">("table");
  const [renaming, setRenaming] = useState<string | null>(null);
  const [split, setSplit] = useState(() => Number(localStorage.getItem(LAYOUT_KEY)) || 52);
  const [showSchema, setShowSchema] = useState(true);
  const editor = useRef<CodeEditorHandle>(null);
  const stop = useRef(false);
  const workspace = useRef<HTMLDivElement>(null);

  const active = library.scripts.find((script) => script.id === library.active) ?? library.scripts[0];

  useEffect(() => {
    const timer = window.setTimeout(() => saveLibrary(library), 300);
    return () => window.clearTimeout(timer);
  }, [library]);

  const update = (patch: Partial<Script>, id = active.id) =>
    setLibrary((current) => ({
      ...current,
      scripts: current.scripts.map((script) => (script.id === id ? { ...script, ...patch } : script)),
    }));

  const addScript = (name: string, text: string) => {
    const id = newId();
    setLibrary((current) => ({ scripts: [...current.scripts, { id, name, text }], active: id }));
    setRuns([]);
  };

  const closeScript = async (script: Script) => {
    if (script.text.trim() && !(await confirm({ title: `Delete the script “${script.name}”?`, message: "Its text is lost unless you saved it to a file.", confirm: "Delete", danger: true }))) {
      return;
    }
    setLibrary((current) => {
      const scripts = current.scripts.filter((item) => item.id !== script.id);
      if (scripts.length === 0) scripts.push({ id: newId(), name: "Script 1", text: "" });
      const active = current.active === script.id ? scripts[Math.max(0, current.scripts.indexOf(script) - 1)]?.id ?? scripts[0].id : current.active;
      return { scripts, active };
    });
  };

  const labels = useMemo(() => ({ labels: Object.keys(schema.labels), edgeLabels: Object.keys(schema.edgeLabels) }), [schema]);

  const execute = useCallback(
    async (all: boolean) => {
      if (running) return;
      const script = active.text;
      const selection = editor.current?.selection();
      let statements: Statement[];
      if (selection) {
        statements = splitStatements(selection.text).map((statement) => {
          const from = statement.from + selection.from;
          return { ...statement, from, to: statement.to + selection.from, line: lineOf(script, from) };
        });
      } else if (all) {
        statements = splitStatements(script);
      } else {
        const statement = statementAt(script, editor.current?.cursor() ?? 0);
        statements = statement ? [statement] : [];
      }
      if (statements.length === 0) {
        toast("Nothing to run: write a statement first.", "info");
        return;
      }
      setRunning(true);
      stop.current = false;
      const graph = client.namespace(namespace);
      const done: Run[] = [];
      let wrote = false;
      setRuns([]);
      setTab("messages");
      for (const statement of statements) {
        if (stop.current) break;
        editor.current?.mark({ running: [statement.from, statement.to] });
        const started = performance.now();
        try {
          const result = await graph.query(statement.text.replace(/;\s*$/, ""));
          done.push({ statement, status: "ok", result, ms: performance.now() - started });
          if (changes(result.stats)) wrote = true;
        } catch (problem) {
          const error = describeError(problem);
          const position = errorPosition(error);
          let at: Run["at"];
          if (position) {
            const lineStart = script.lastIndexOf("\n", statement.from - 1) + 1;
            at = {
              line: statement.line + position.line - 1,
              column: position.line === 1 ? statement.from - lineStart + position.column : position.column,
            };
          }
          done.push({ statement, status: "error", error, at, ms: performance.now() - started });
          setRuns([...done]);
          if (stopOnError) break;
        }
        setRuns([...done]);
      }
      const failed = done.find((run) => run.status === "error");
      editor.current?.mark(failed ? { error: failed.at?.line ?? failed.statement.line } : null);
      if (failed?.at) editor.current?.goto(failed.at.line, failed.at.column);
      // Show the last result that has rows, unless something failed.
      const lastTable = [...done.keys()].reverse().find((index) => (done[index].result?.columns.length ?? 0) > 0);
      setTab(failed || lastTable === undefined ? "messages" : lastTable);
      if (wrote) changed();
      setRunning(false);
    },
    [running, active.text, client, namespace, stopOnError, changed, toast],
  );

  const startDrag = (event: React.PointerEvent) => {
    event.preventDefault();
    const box = workspace.current?.getBoundingClientRect();
    if (!box) return;
    const move = (e: PointerEvent) => {
      const percent = Math.min(85, Math.max(15, ((e.clientY - box.top) / box.height) * 100));
      setSplit(percent);
    };
    const up = () => {
      window.removeEventListener("pointermove", move);
      window.removeEventListener("pointerup", up);
      setSplit((value) => {
        try {
          localStorage.setItem(LAYOUT_KEY, String(Math.round(value)));
        } catch {
          // Only a convenience.
        }
        return value;
      });
    };
    window.addEventListener("pointermove", move);
    window.addEventListener("pointerup", up);
  };

  const tables = runs.map((run, index) => ({ run, index })).filter(({ run }) => (run.result?.columns.length ?? 0) > 0);
  const current = typeof tab === "number" ? runs[tab]?.result : undefined;
  const total = runs.reduce((sum, run) => sum + run.ms, 0);
  const openNode = useCallback((id: string) => go("explorer", id), []);

  return (
    <div className="scripts">
      {showSchema && (
        <aside className="schema">
          <div className="schema-head">
            <span>Schema</span>
            <span className="muted small">{namespace}</span>
          </div>
          <SchemaGroup title="Node labels" empty="No nodes yet.">
            {Object.entries(schema.labels)
              .sort((a, b) => b[1] - a[1])
              .map(([label, count]) => (
                <SchemaItem
                  key={label}
                  color={labelColor(label)}
                  name={label}
                  count={count}
                  onInsert={() => editor.current?.insert(`:${label}`)}
                  onQuery={() => editor.current?.insert(`\nMATCH (n:${label})\nRETURN n\nLIMIT 100;\n`)}
                />
              ))}
          </SchemaGroup>
          <SchemaGroup title="Edge labels" empty="No facts yet.">
            {Object.entries(schema.edgeLabels)
              .sort((a, b) => b[1] - a[1])
              .map(([label, count]) => (
                <SchemaItem
                  key={label}
                  name={label}
                  count={count}
                  mono
                  onInsert={() => editor.current?.insert(`:${label}`)}
                  onQuery={() => editor.current?.insert(`\nMATCH (a)-[r:${label}]->(b)\nRETURN a, r, b\nLIMIT 100;\n`)}
                />
              ))}
          </SchemaGroup>
          <SchemaGroup title="Snippets">
            {SNIPPETS.map((snippet) => (
              <button key={snippet.name} className="schema-item" onClick={() => editor.current?.insert(`\n${snippet.text}\n`)} title={snippet.text}>
                <Icon name="file" size={13} className="faint" />
                <span className="grow-text">{snippet.name}</span>
              </button>
            ))}
          </SchemaGroup>
          <SchemaGroup title="Pseudo-properties" collapsed>
            {PSEUDO_PROPERTIES.map((name) => (
              <button key={name} className="schema-item mono" onClick={() => editor.current?.insert(`.${name}`)}>
                {name}
              </button>
            ))}
          </SchemaGroup>
        </aside>
      )}

      <div className="workspace" ref={workspace}>
        <div className="script-tabs">
          <button className="ghost icon-button" onClick={() => setShowSchema((value) => !value)} title={showSchema ? "Hide schema" : "Show schema"}>
            <Icon name={showSchema ? "chevronLeft" : "chevronRight"} />
          </button>
          {library.scripts.map((script) => (
            <div
              key={script.id}
              className={script.id === active.id ? "script-tab active" : "script-tab"}
              onClick={() => {
                if (script.id !== active.id) {
                  setLibrary((current) => ({ ...current, active: script.id }));
                  setRuns([]);
                }
              }}
              onDoubleClick={() => setRenaming(script.id)}
              title="Double-click to rename"
            >
              <Icon name="file" size={13} />
              {renaming === script.id ? (
                <input
                  autoFocus
                  defaultValue={script.name}
                  onBlur={(event) => {
                    update({ name: event.target.value.trim() || script.name }, script.id);
                    setRenaming(null);
                  }}
                  onKeyDown={(event) => {
                    if (event.key === "Enter") event.currentTarget.blur();
                    if (event.key === "Escape") setRenaming(null);
                  }}
                />
              ) : (
                <span>{script.name}</span>
              )}
              <button
                className="tab-x"
                onClick={(event) => {
                  event.stopPropagation();
                  void closeScript(script);
                }}
                aria-label={`Close ${script.name}`}
              >
                <Icon name="x" size={12} />
              </button>
            </div>
          ))}
          <button className="ghost icon-button" onClick={() => addScript(`Script ${library.scripts.length + 1}`, "")} title="New script">
            <Icon name="plus" />
          </button>
        </div>

        <div className="script-toolbar">
          {running ? (
            <button className="danger" onClick={() => (stop.current = true)}>
              <Icon name="x" /> Stop
            </button>
          ) : (
            <>
              <button className="primary" onClick={() => void execute(false)} title="Run the statement under the cursor, or the selection (Ctrl+Enter)">
                <Icon name="play" /> Run <kbd>Ctrl ⏎</kbd>
              </button>
              <button onClick={() => void execute(true)} title="Run every statement (F5)">
                <Icon name="playAll" /> Run all <kbd>F5</kbd>
              </button>
            </>
          )}
          <label className="check small">
            <input type="checkbox" checked={stopOnError} onChange={(event) => setStopOnError(event.target.checked)} /> Stop on error
          </label>
          <div className="spacer" />
          <label className="ghost button" title="Open a .gq file">
            <Icon name="folder" /> Open
            <input
              type="file"
              accept=".gq,.cypher,.cql,.txt"
              hidden
              onChange={(event) => {
                const file = event.target.files?.[0];
                event.target.value = "";
                if (file) void file.text().then((text) => addScript(file.name.replace(/\.[^.]+$/, ""), text));
              }}
            />
          </label>
          <button className="ghost" onClick={() => download(`${active.name}.gq`, active.text, "text/plain")} title="Download as a .gq file">
            <Icon name="save" /> Save
          </button>
          <a className="ghost button" href="https://github.com/Seerendo/grafeno/blob/main/docs/query-language.md" target="_blank" rel="noreferrer">
            Reference
          </a>
        </div>

        <div className="editor-pane" style={{ height: `${split}%` }}>
          <CodeEditor ref={editor} value={active.text} onChange={(text) => update({ text })} onRun={(all) => void execute(all)} schema={() => labels} />
        </div>

        <div className="splitter" onPointerDown={startDrag} role="separator" aria-orientation="horizontal" />

        <div className="results">
          <div className="result-tabs">
            <button className={tab === "messages" ? "result-tab active" : "result-tab"} onClick={() => setTab("messages")}>
              Messages
              {runs.some((run) => run.status === "error") && <span className="badge danger">!</span>}
            </button>
            {tables.map(({ run, index }, position) => (
              <button key={index} className={tab === index ? "result-tab active" : "result-tab"} onClick={() => setTab(index)}>
                Result {position + 1} <span className="badge">{run.result!.rows.length}</span>
              </button>
            ))}
            <div className="spacer" />
            {current && (
              <>
                <div className="segmented">
                  <button className={mode === "table" ? "active" : ""} onClick={() => setMode("table")}>
                    <Icon name="table" size={14} /> Table
                  </button>
                  <button className={mode === "json" ? "active" : ""} onClick={() => setMode("json")}>
                    <Icon name="braces" size={14} /> JSON
                  </button>
                  <button className={mode === "graph" ? "active" : ""} onClick={() => setMode("graph")} disabled={!hasGraph(current.rows)}>
                    <Icon name="graph" size={14} /> Graph
                  </button>
                </div>
                <button
                  className="ghost small-button"
                  onClick={() =>
                    download(
                      `${active.name}.csv`,
                      [current.columns.map(csvCell).join(","), ...current.rows.map((row) => row.map(csvCell).join(","))].join("\n"),
                      "text/csv",
                    )
                  }
                >
                  <Icon name="download" size={14} /> CSV
                </button>
                <button className="ghost small-button" onClick={() => download(`${active.name}.json`, JSON.stringify(current, null, 2), "application/json")}>
                  <Icon name="download" size={14} /> JSON
                </button>
              </>
            )}
          </div>
          <div className="result-body">
            {tab === "messages" ? (
              runs.length === 0 ? (
                <div className="muted pad small">{running ? "Running…" : "Results and messages appear here."}</div>
              ) : (
                <div className="messages">
                  {runs.map((run, index) => (
                    <button
                      key={index}
                      className={run.status === "error" ? "message error-message" : "message"}
                      onClick={() => {
                        if (run.status === "error" && run.at) editor.current?.goto(run.at.line, run.at.column);
                        else if (run.result?.columns.length) setTab(index);
                        else editor.current?.goto(run.statement.line, 1);
                      }}
                    >
                      <Icon name={run.status === "error" ? "x" : "check"} size={14} />
                      <span className="mono faint">line {run.at?.line ?? run.statement.line}</span>
                      <span className="grow-text">
                        {run.status === "error"
                          ? run.error
                          : [run.result!.columns.length ? `${run.result!.rows.length} rows` : "", changes(run.result!.stats)].filter(Boolean).join(" · ") || "Done"}
                      </span>
                      <span className="muted small mono">{run.ms.toFixed(0)} ms</span>
                    </button>
                  ))}
                  {!running && (
                    <div className="muted small pad">
                      {runs.length} statement{runs.length === 1 ? "" : "s"} · {total.toFixed(0)} ms
                    </div>
                  )}
                </div>
              )
            ) : current ? (
              mode === "json" ? (
                <pre className="json-view">{JSON.stringify(current.rows, null, 2)}</pre>
              ) : mode === "graph" && hasGraph(current.rows) ? (
                <MiniGraph rows={current.rows} onOpen={openNode} />
              ) : (
                <ResultTable columns={current.columns} rows={current.rows} />
              )
            ) : null}
          </div>
        </div>
      </div>
    </div>
  );
}

function SchemaGroup({ title, children, empty, collapsed }: { title: string; children: React.ReactNode; empty?: string; collapsed?: boolean }) {
  const [open, setOpen] = useState(!collapsed);
  const items = Array.isArray(children) ? children.flat() : [children];
  return (
    <div className="schema-group">
      <button className="schema-title" onClick={() => setOpen((value) => !value)}>
        <Icon name={open ? "chevronDown" : "chevronRight"} size={13} />
        {title}
      </button>
      {open && (items.filter(Boolean).length === 0 ? <div className="muted small schema-empty">{empty}</div> : children)}
    </div>
  );
}

function SchemaItem({
  name,
  count,
  color,
  mono,
  onInsert,
  onQuery,
}: {
  name: string;
  count: number;
  color?: string;
  mono?: boolean;
  onInsert: () => void;
  onQuery: () => void;
}) {
  return (
    <div className="schema-item">
      <button className="schema-name" onClick={onInsert} title={`Insert :${name}`}>
        {color ? <span className="dot" style={{ background: color }} /> : <span className="faint mono">→</span>}
        <span className={mono ? "mono grow-text" : "grow-text"}>{name}</span>
        <span className="faint small">{count}</span>
      </button>
      <button className="ghost icon-button" onClick={onQuery} title="Insert a query for it">
        <Icon name="play" size={12} />
      </button>
    </div>
  );
}

