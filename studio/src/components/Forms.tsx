import { useState } from "react";

import { describeError } from "../api.ts";
import { type NewEdgeOptions, type NewNodeOptions, useStudio } from "../context.ts";
import { Icon } from "../icons.tsx";
import { LabelInput, Modal, NodePicker, type Picked, PropsEditor, type PropRow, propsFromRows } from "../ui.tsx";

/** Creates a node: labels, name, properties, searchable text and importance. */
export function NodeForm({ options, onClose }: { options: NewNodeOptions; onClose: () => void }) {
  const { client, namespace, schema, changed, toast } = useStudio();
  const [labels, setLabels] = useState<string[]>(options.labels ?? []);
  const [name, setName] = useState("");
  const [rows, setRows] = useState<PropRow[]>([]);
  const [text, setText] = useState("");
  const [importance, setImportance] = useState(0.5);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const save = async () => {
    setError(null);
    try {
      if (labels.length === 0) throw new Error("Give the node at least one label, such as Person or Note.");
      const props = propsFromRows(rows);
      if (name.trim()) props.name = name.trim();
      setSaving(true);
      const graph = client.namespace(namespace);
      const { id } = await graph.createNode({ labels, props, text: text.trim() || undefined });
      if (importance !== 0.5) await graph.patchNode(id, { importance });
      changed();
      toast(`Created ${name.trim() || labels[0]}.`, "ok");
      onClose();
      options.onCreated?.(id);
    } catch (problem) {
      setError(describeError(problem));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      title="New node"
      onClose={onClose}
      footer={
        <>
          <span className="muted small">in namespace “{namespace}”</span>
          <div className="spacer" />
          <button type="button" onClick={onClose}>Cancel</button>
          <button type="button" className="primary" disabled={saving} onClick={() => void save()}>
            <Icon name="check" /> Create node
          </button>
        </>
      }
    >
      <form
        className="form"
        onSubmit={(event) => {
          event.preventDefault();
          void save();
        }}
      >
        <label className="field">
          Labels <span className="hint">what kind of thing it is</span>
          <LabelInput value={labels} onChange={setLabels} suggestions={Object.keys(schema.labels)} placeholder="Person, Note, Project…" />
        </label>
        <label className="field">
          Name
          <input value={name} onChange={(event) => setName(event.target.value)} placeholder="Ana García" autoFocus />
        </label>
        <div className="field">
          Properties
          <PropsEditor rows={rows} onChange={setRows} />
        </div>
        <label className="field">
          Text <span className="hint">searchable by full text and recall</span>
          <textarea rows={3} value={text} onChange={(event) => setText(event.target.value)} placeholder="What to remember about it…" />
        </label>
        <label className="field">
          Importance <span className="hint">{importance.toFixed(2)} · ranks higher and fades slower</span>
          <input type="range" min={0} max={1} step={0.05} value={importance} onChange={(event) => setImportance(Number(event.target.value))} />
        </label>
        {error && <div className="error">{error}</div>}
        <button type="submit" hidden />
      </form>
    </Modal>
  );
}

/** Creates a fact: an edge between two nodes, with the dates it holds. */
export function EdgeForm({ options, onClose }: { options: NewEdgeOptions; onClose: () => void }) {
  const { client, namespace, schema, changed, toast } = useStudio();
  const [from, setFrom] = useState<Picked | null>(options.from ?? null);
  const [to, setTo] = useState<Picked | null>(options.to ?? null);
  const [label, setLabel] = useState<string[]>([]);
  const [validFrom, setValidFrom] = useState("");
  const [validTo, setValidTo] = useState("");
  const [rows, setRows] = useState<PropRow[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  const save = async () => {
    setError(null);
    try {
      if (!from || !to) throw new Error("Choose both nodes.");
      if (label.length === 0) throw new Error("Name the relation, such as works_at or knows.");
      setSaving(true);
      const { id } = await client.namespace(namespace).createEdge({
        from: from.id,
        label: label[0],
        to: to.id,
        props: propsFromRows(rows),
        valid_from: validFrom || undefined,
        valid_to: validTo || undefined,
      });
      changed();
      toast(`${from.name} —${label[0]}→ ${to.name}`, "ok");
      onClose();
      options.onCreated?.(id);
    } catch (problem) {
      setError(describeError(problem));
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      title="New fact"
      onClose={onClose}
      footer={
        <>
          <span className="muted small">in namespace “{namespace}”</span>
          <div className="spacer" />
          <button type="button" onClick={onClose}>Cancel</button>
          <button type="button" className="primary" disabled={saving} onClick={() => void save()}>
            <Icon name="link" /> Create fact
          </button>
        </>
      }
    >
      <div className="form">
        <div className="fact-builder">
          <div className="field">
            From
            <NodePicker value={from} onChange={setFrom} autoFocus={!from} />
          </div>
          <div className="field">
            Relation
            <LabelInput value={label} onChange={setLabel} suggestions={Object.keys(schema.edgeLabels)} placeholder="works_at" single />
          </div>
          <div className="field">
            To
            <NodePicker value={to} onChange={setTo} autoFocus={!!from && !to} />
          </div>
        </div>
        <button type="button"
          className="ghost small-button"
          onClick={() => {
            setFrom(to);
            setTo(from);
          }}
        >
          ⇄ Swap direction
        </button>
        <div className="grid-2">
          <label className="field">
            Valid from <span className="hint">when it became true</span>
            <input type="date" value={validFrom} onChange={(event) => setValidFrom(event.target.value)} />
          </label>
          <label className="field">
            Valid until <span className="hint">empty: still true</span>
            <input type="date" value={validTo} onChange={(event) => setValidTo(event.target.value)} />
          </label>
        </div>
        <div className="field">
          Properties
          <PropsEditor rows={rows} onChange={setRows} />
        </div>
        {error && <div className="error">{error}</div>}
      </div>
    </Modal>
  );
}
