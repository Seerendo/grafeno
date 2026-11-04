import { useState } from "react";

import { type Node, describeError, displayName, labelColor } from "../api.ts";
import { useStudio } from "../context.ts";
import type { Context, Recalled } from "../../../clients/typescript/src/index.ts";

type Mode = "text" | "recall" | "context";

const MODES: { id: Mode; title: string; help: string }[] = [
  { id: "text", title: "Full text", help: "Words in the nodes' text (BM25), without accents or case." },
  { id: "recall", title: "Recall", help: "Text and vectors fused, then spread through the graph: what an agent would find." },
  { id: "context", title: "Context", help: "The memories and facts an agent would get in its prompt, within a token budget." },
];

function NodeLink({ node }: { node: Node }) {
  return (
    <a href={`#/explorer/${node.id}`}>
      <span className="dot" style={{ background: labelColor(node.labels[0] ?? "") }} /> {displayName(node)}
    </a>
  );
}

export function SearchView() {
  const { client, namespace } = useStudio();
  const [mode, setMode] = useState<Mode>("recall");
  const [text, setText] = useState("");
  const [budget, setBudget] = useState(800);
  const [hits, setHits] = useState<{ node: Node; score: number }[] | null>(null);
  const [recalled, setRecalled] = useState<Recalled[] | null>(null);
  const [context, setContext] = useState<Context | null>(null);
  const [error, setError] = useState<string | null>(null);

  const search = async () => {
    if (!text.trim()) return;
    const graph = client.namespace(namespace);
    setError(null);
    setHits(null);
    setRecalled(null);
    setContext(null);
    try {
      if (mode === "text") {
        const found = await graph.searchText(text, 20);
        const nodes = await Promise.all(found.map((hit) => graph.getNode(hit.node)));
        setHits(found.map((hit, index) => ({ node: nodes[index], score: hit.score })));
      } else if (mode === "recall") {
        setRecalled(await graph.recall({ text, limit: 20 }));
      } else {
        setContext(await graph.context({ text, budget, limit: 20 }));
      }
    } catch (problem) {
      setError(describeError(problem));
    }
  };

  return (
    <div className="stack">
      <h1>Search</h1>
      <div className="panel stack">
        <div className="chips">
          {MODES.map((item) => (
            <button key={item.id} className={mode === item.id ? "chip active" : "chip"} onClick={() => setMode(item.id)}>
              {item.title}
            </button>
          ))}
        </div>
        <div className="muted small">{MODES.find((item) => item.id === mode)?.help}</div>
        <div className="row">
          <input
            className="grow"
            value={text}
            onChange={(e) => setText(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && void search()}
            placeholder={mode === "context" ? "A question an agent would ask" : "What are you looking for?"}
          />
          {mode === "context" && (
            <label className="row small">
              Budget
              <input type="number" min={50} step={50} value={budget} onChange={(e) => setBudget(Number(e.target.value))} style={{ width: 90 }} />
              tokens
            </label>
          )}
          <button className="primary" onClick={() => void search()}>Search</button>
        </div>
      </div>
      {error && <div className="error">{error}</div>}

      {hits && (
        <div className="table-wrap">
          <table>
            <thead>
              <tr><th>Node</th><th>Labels</th><th>Score</th></tr>
            </thead>
            <tbody>
              {hits.length === 0 && <tr><td colSpan={3} className="muted">No matches.</td></tr>}
              {hits.map(({ node, score }) => (
                <tr key={node.id}>
                  <td><NodeLink node={node} /></td>
                  <td className="muted">{node.labels.join(", ")}</td>
                  <td className="mono">{score.toFixed(3)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {recalled && (
        <div className="table-wrap">
          <table>
            <thead>
              <tr><th>Node</th><th>Why</th><th>Score</th></tr>
            </thead>
            <tbody>
              {recalled.length === 0 && <tr><td colSpan={3} className="muted">Nothing recalled.</td></tr>}
              {recalled.map((result) => (
                <tr key={result.node.id}>
                  <td><NodeLink node={result.node} /></td>
                  <td className="small">
                    {result.signals
                      .map((signal) =>
                        signal.kind === "text"
                          ? `text #${signal.rank}`
                          : signal.kind === "vector"
                            ? `meaning #${signal.rank} (${signal.similarity.toFixed(2)})`
                            : signal.kind === "graph"
                              ? `${signal.hops} hop(s) from a match`
                              : `importance ${signal.importance.toFixed(2)}, recency ${signal.recency.toFixed(2)}`,
                      )
                      .join(" · ")}
                  </td>
                  <td className="mono">{result.score.toFixed(4)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}

      {context && (
        <div className="panel stack">
          <div className="muted small">
            {context.items.length} memories · {context.facts.length} facts · {context.tokens} of {budget} tokens
          </div>
          <pre>{context.rendered || "No relevant memories."}</pre>
        </div>
      )}
    </div>
  );
}
