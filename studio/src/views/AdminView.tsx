import { useState } from "react";

import { describeError } from "../api.ts";
import { IndexesPanel } from "../components/IndexesPanel.tsx";
import { useStudio } from "../context.ts";
import type { CheckReport, DumpStats, RebuildReport } from "../../../clients/typescript/src/index.ts";

function download(name: string, text: string): void {
  const url = URL.createObjectURL(new Blob([text], { type: "application/x-ndjson" }));
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}

export function AdminView() {
  const { client, namespace, changed: refreshNamespaces, confirm } = useStudio();
  const [check, setCheck] = useState<CheckReport | null>(null);
  const [rebuild, setRebuild] = useState<RebuildReport | null>(null);
  const [imported, setImported] = useState<DumpStats | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const task = async (name: string, work: () => Promise<void>) => {
    setBusy(name);
    setError(null);
    try {
      await work();
    } catch (problem) {
      setError(describeError(problem));
    } finally {
      setBusy(null);
    }
  };

  const day = new Date().toISOString().slice(0, 10);

  return (
    <div className="stack">
      <h1>Administration</h1>
      {error && <div className="error">{error}</div>}

      <IndexesPanel />

      <div className="panel stack">
        <h2>Integrity of “{namespace}”</h2>
        <p className="muted small" style={{ margin: 0 }}>
          Checks that every index and counter agrees with the records, and that searches can reach every vector.
        </p>
        <div className="row">
          <button disabled={busy !== null} onClick={() => void task("check", async () => setCheck(await client.namespace(namespace).check()))}>
            {busy === "check" ? "Checking…" : "Check"}
          </button>
          <button
            disabled={busy !== null}
            onClick={async () => {
              if (!(await confirm({ title: `Rebuild the indexes of “${namespace}”?`, message: "Every index and counter is rebuilt from the records. Reads keep working.", confirm: "Rebuild" }))) return;
              void task("rebuild", async () => setRebuild(await client.namespace(namespace).rebuild()));
            }}
          >
            {busy === "rebuild" ? "Rebuilding…" : "Rebuild indexes"}
          </button>
        </div>
        {check && (
          <div className="small">
            <div className={check.problems.length === 0 ? "ok" : "error"}>
              {check.problems.length === 0 ? "Consistent." : `${check.problems.length} problems:`}
            </div>
            <div className="muted">
              {check.nodes} nodes · {check.edges} edges · {check.texts} texts · {check.vectors} vectors · {check.dangling_links} dangling links
            </div>
            {check.problems.length > 0 && <pre>{check.problems.join("\n")}</pre>}
          </div>
        )}
        {rebuild && <pre className="small">{JSON.stringify(rebuild, null, 2)}</pre>}
      </div>

      <div className="panel stack">
        <h2>Export and import</h2>
        <p className="muted small" style={{ margin: 0 }}>
          A JSON Lines file with nodes (text and embeddings included) and edges, keeping ids and times. Importing is all or
          nothing, and replaces records with the same ids.
        </p>
        <div className="row">
          <button
            disabled={busy !== null}
            onClick={() => void task("export", async () => download(`grafeno-${namespace}-${day}.jsonl`, await client.export([namespace])))}
          >
            Export “{namespace}”
          </button>
          <button
            disabled={busy !== null}
            onClick={() => void task("export", async () => download(`grafeno-all-${day}.jsonl`, await client.export()))}
          >
            Export everything
          </button>
          <label className="button">
            Import a file…
            <input
              type="file"
              accept=".jsonl,.ndjson,application/x-ndjson"
              hidden
              onChange={(event) => {
                const file = event.target.files?.[0];
                event.target.value = "";
                if (!file) return;
                void task("import", async () => {
                  setImported(await client.import(await file.text()));
                  refreshNamespaces();
                });
              }}
            />
          </label>
        </div>
        {imported && (
          <div className="ok small">
            Imported {imported.namespaces} namespaces, {imported.nodes} nodes and {imported.edges} edges.
          </div>
        )}
      </div>
    </div>
  );
}
