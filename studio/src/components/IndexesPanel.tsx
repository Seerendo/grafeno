import { useCallback, useEffect, useState } from "react";

import { describeError } from "../api.ts";
import { useStudio } from "../context.ts";
import { Icon } from "../icons.tsx";
import { LabelInput } from "../ui.tsx";

interface IndexRow {
  name: string;
  label: string;
  property: string;
  unique: boolean;
  entries: number;
}

/** Property indexes and unique constraints of the namespace. */
export function IndexesPanel() {
  const { client, namespace, schema, confirm, toast, revision } = useStudio();
  const graph = client.namespace(namespace);
  const [indexes, setIndexes] = useState<IndexRow[]>([]);
  const [label, setLabel] = useState<string[]>([]);
  const [property, setProperty] = useState("");
  const [unique, setUnique] = useState(false);
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    try {
      const result = await graph.query("SHOW INDEXES");
      setIndexes(
        result.rows.map(([name, label, property, unique, entries]) => ({
          name: String(name),
          label: String(label),
          property: String(property),
          unique: Boolean(unique),
          entries: Number(entries),
        })),
      );
    } catch (problem) {
      toast(describeError(problem), "error");
    }
    // `graph` changes identity on every render.
  }, [client, namespace, toast]);

  useEffect(() => {
    void load();
  }, [load, revision]);

  const create = async () => {
    const key = property.trim();
    if (!label[0] || !/^[A-Za-z_][\w]*$/.test(key)) {
      toast("Choose a label and a property name (letters, digits and _).", "error");
      return;
    }
    setBusy(true);
    try {
      const statement = unique
        ? `CREATE CONSTRAINT FOR (n:\`${label[0]}\`) REQUIRE n.${key} IS UNIQUE`
        : `CREATE INDEX FOR (n:\`${label[0]}\`) ON (n.${key})`;
      const result = await graph.query(statement);
      toast(`Indexed ${result.rows[0]?.[4] ?? 0} nodes.`, "ok");
      setProperty("");
      await load();
    } catch (problem) {
      toast(describeError(problem), "error");
    } finally {
      setBusy(false);
    }
  };

  const drop = async (index: IndexRow) => {
    const ok = await confirm({
      title: `Drop ${index.unique ? "the constraint" : "the index"} ${index.name}?`,
      message: index.unique
        ? "Duplicate values will be allowed again. The data stays."
        : "Queries on this property will scan the label again. The data stays.",
      confirm: "Drop",
      danger: true,
    });
    if (!ok) return;
    try {
      await graph.query(`DROP ${index.unique ? "CONSTRAINT" : "INDEX"} \`${index.name}\``);
      await load();
    } catch (problem) {
      toast(describeError(problem), "error");
    }
  };

  return (
    <div className="panel stack">
      <h2>Indexes of “{namespace}”</h2>
      <p className="muted small" style={{ margin: 0 }}>
        An index on a label and property makes queries such as <code>WHERE n.age &gt; 32</code> or{" "}
        <code>{"{email: $email}"}</code> read only the matching nodes. A unique constraint also refuses duplicate values.
      </p>
      {indexes.length === 0 ? (
        <div className="muted small">No indexes yet: every query by property scans its label.</div>
      ) : (
        <div className="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>On</th>
                <th>Kind</th>
                <th>Nodes</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {indexes.map((index) => (
                <tr key={index.name}>
                  <td className="mono">{index.name}</td>
                  <td className="mono">
                    :{index.label}({index.property})
                  </td>
                  <td>{index.unique ? "unique" : "index"}</td>
                  <td className="mono">{index.entries}</td>
                  <td style={{ textAlign: "right" }}>
                    <button className="ghost icon-button danger" onClick={() => void drop(index)} title="Drop">
                      <Icon name="trash" size={14} />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      )}
      <div className="index-form">
        <div className="field">
          Label
          <LabelInput value={label} onChange={setLabel} suggestions={Object.keys(schema.labels)} placeholder="Person" single />
        </div>
        <label className="field">
          Property
          <input value={property} onChange={(event) => setProperty(event.target.value)} placeholder="age" />
        </label>
        <label className="check small">
          <input type="checkbox" checked={unique} onChange={(event) => setUnique(event.target.checked)} /> Unique
        </label>
        <button className="primary" disabled={busy} onClick={() => void create()}>
          <Icon name="plus" /> {unique ? "Add constraint" : "Add index"}
        </button>
      </div>
    </div>
  );
}
