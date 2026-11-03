import { useState } from "react";

import { type QueryResult, describeError } from "../api.ts";
import { useStudio } from "../App.tsx";
import { ResultTable } from "../components/ResultTable.tsx";

const EXAMPLES: Record<string, string> = {
  "Everything (small graphs)": "MATCH (n)\nRETURN n, labels(n)\nLIMIT 100",
  "Nodes per label": "MATCH (n)\nRETURN labels(n) AS labels, count(*) AS nodes\nORDER BY nodes DESC",
  "All facts": "MATCH (a)-[r]->(b)\nRETURN a, type(r) AS fact, b, r.@valid_from, r.@valid_to\nLIMIT 100",
  "As of a date": "MATCH (a)-[r]->(b)\nVALID AT '2024-01-01'\nRETURN a, type(r), b",
  "Create": "CREATE (a:Person {name: 'Ana', @text: 'Ana writes Rust'})-[:works_at {@valid_from: '2024-01-01'}]->(c:Company {name: 'Acme'})\nRETURN a, c",
};

const HISTORY_KEY = "grafeno.studio.history";

function loadHistory(): string[] {
  try {
    return JSON.parse(localStorage.getItem(HISTORY_KEY) ?? "[]") as string[];
  } catch {
    return [];
  }
}

function saveHistory(history: string[]): void {
  try {
    localStorage.setItem(HISTORY_KEY, JSON.stringify(history));
  } catch {
    // History is only a convenience.
  }
}

function changes(stats: QueryResult["stats"]): string {
  return Object.entries(stats)
    .filter(([, count]) => count > 0)
    .map(([key, count]) => `${count} ${key.replace("_", " ")}`)
    .join(", ");
}

export function QueryView() {
  const { client, namespace, refreshNamespaces } = useStudio();
  const [query, setQuery] = useState(() => loadHistory()[0] ?? EXAMPLES["Nodes per label"]);
  const [result, setResult] = useState<QueryResult | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [running, setRunning] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [asJson, setAsJson] = useState(false);
  const [history, setHistory] = useState(loadHistory);

  const run = async () => {
    const statement = query.trim().replace(/;\s*$/, "");
    if (!statement) return;
    setRunning(true);
    const started = performance.now();
    try {
      const output = await client.namespace(namespace).query(statement);
      setResult(output);
      setError(null);
      const updated = [statement, ...history.filter((item) => item !== statement)].slice(0, 30);
      setHistory(updated);
      saveHistory(updated);
      if (changes(output.stats)) refreshNamespaces();
    } catch (problem) {
      setError(describeError(problem));
      setResult(null);
    } finally {
      setElapsed(performance.now() - started);
      setRunning(false);
    }
  };

  return (
    <div className="stack">
      <h1>Query</h1>
      <div className="panel stack">
        <textarea
          className="code"
          rows={8}
          value={query}
          spellCheck={false}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && (e.ctrlKey || e.metaKey)) {
              e.preventDefault();
              void run();
            }
          }}
          aria-label="GQ query"
        />
        <div className="row">
          <button className="primary" disabled={running} onClick={() => void run()}>
            {running ? "Running…" : "Run"}
          </button>
          <span className="muted small">Ctrl+Enter</span>
          <select value="" onChange={(e) => e.target.value && setQuery(EXAMPLES[e.target.value])} aria-label="Examples">
            <option value="">Examples…</option>
            {Object.keys(EXAMPLES).map((name) => (
              <option key={name} value={name}>{name}</option>
            ))}
          </select>
          {history.length > 0 && (
            <select value="" onChange={(e) => e.target.value && setQuery(e.target.value)} aria-label="History" style={{ maxWidth: 260 }}>
              <option value="">History…</option>
              {history.map((item) => (
                <option key={item} value={item}>{item.replace(/\s+/g, " ").slice(0, 80)}</option>
              ))}
            </select>
          )}
          <div className="spacer" style={{ flex: 1 }} />
          <label className="row small">
            <input type="checkbox" checked={asJson} onChange={(e) => setAsJson(e.target.checked)} /> JSON
          </label>
          <a className="small" href="https://github.com/Seerendo/grafeno/blob/main/docs/query-language.md" target="_blank" rel="noreferrer">
            Language reference
          </a>
        </div>
      </div>
      {error && <div className="error">{error}</div>}
      {result && (
        <div className="stack">
          <div className="muted small">
            {result.columns.length > 0 && `${result.rows.length} rows · `}
            {changes(result.stats) && `${changes(result.stats)} · `}
            {elapsed.toFixed(0)} ms
          </div>
          {asJson ? <pre>{JSON.stringify(result, null, 2)}</pre> : <ResultTable columns={result.columns} rows={result.rows} />}
        </div>
      )}
    </div>
  );
}
