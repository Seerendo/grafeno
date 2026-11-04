// Building blocks shared by the views: dialogs, label and property editors,
// a node picker.

import { type ReactNode, useEffect, useId, useRef, useState } from "react";

import { type Node, describeError, displayName, labelColor } from "./api.ts";
import { useStudio } from "./context.ts";
import { Icon } from "./icons.tsx";

export function Modal({
  title,
  children,
  footer,
  onClose,
  wide,
}: {
  title: string;
  children: ReactNode;
  footer?: ReactNode;
  onClose: () => void;
  wide?: boolean;
}) {
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => event.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="backdrop" onMouseDown={(event) => event.target === event.currentTarget && onClose()}>
      <div className={wide ? "modal wide" : "modal"} role="dialog" aria-modal="true" aria-label={title}>
        <header>
          <h2>{title}</h2>
          <button type="button" className="ghost icon-button" onClick={onClose} aria-label="Close">
            <Icon name="x" />
          </button>
        </header>
        <div className="modal-body">{children}</div>
        {footer && <footer>{footer}</footer>}
      </div>
    </div>
  );
}

/** A label: a colored dot and the name. */
export function LabelChip({ label, onRemove }: { label: string; onRemove?: () => void }) {
  return (
    <span className="chip label-chip" style={{ ["--label" as string]: labelColor(label) }}>
      <span className="dot" />
      {label}
      {onRemove && (
        <button type="button" className="chip-x" onClick={onRemove} aria-label={`Remove ${label}`}>
          <Icon name="x" size={12} />
        </button>
      )}
    </span>
  );
}

/** Labels as chips, with a field that suggests the namespace's labels. */
export function LabelInput({
  value,
  onChange,
  suggestions,
  placeholder = "Add a label…",
  single,
}: {
  value: string[];
  onChange: (labels: string[]) => void;
  suggestions: string[];
  placeholder?: string;
  single?: boolean;
}) {
  const [text, setText] = useState("");
  const list = useId();
  const add = (raw: string) => {
    const label = raw.trim().replace(/^:/, "");
    if (!label) return;
    onChange(single ? [label] : value.includes(label) ? value : [...value, label]);
    setText("");
  };
  return (
    <div className="label-input">
      {value.map((label) => (
        <LabelChip key={label} label={label} onRemove={() => onChange(value.filter((item) => item !== label))} />
      ))}
      {(!single || value.length === 0) && (
        <>
          <input
            list={list}
            value={text}
            placeholder={value.length ? "" : placeholder}
            onChange={(event) => {
              const next = event.target.value;
              // Picking a suggestion from the list adds it at once.
              if (suggestions.includes(next)) add(next);
              else setText(next);
            }}
            onKeyDown={(event) => {
              if (event.key === "Enter" || event.key === "," || event.key === "Tab") {
                if (text.trim()) {
                  event.preventDefault();
                  add(text);
                }
              } else if (event.key === "Backspace" && !text && value.length) {
                onChange(value.slice(0, -1));
              }
            }}
            onBlur={() => add(text)}
          />
          <datalist id={list}>
            {suggestions
              .filter((label) => !value.includes(label))
              .map((label) => (
                <option key={label} value={label} />
              ))}
          </datalist>
        </>
      )}
    </div>
  );
}

export type PropType = "text" | "number" | "boolean" | "json";

export interface PropRow {
  key: string;
  type: PropType;
  value: string;
}

export function rowsFromProps(props: Record<string, unknown>): PropRow[] {
  return Object.entries(props).map(([key, value]) => {
    if (typeof value === "string") return { key, type: "text", value };
    if (typeof value === "number") return { key, type: "number", value: String(value) };
    if (typeof value === "boolean") return { key, type: "boolean", value: String(value) };
    return { key, type: "json", value: JSON.stringify(value) };
  });
}

/** The properties the rows describe; throws with a readable message. */
export function propsFromRows(rows: PropRow[]): Record<string, unknown> {
  const props: Record<string, unknown> = {};
  for (const row of rows) {
    const key = row.key.trim();
    if (!key) {
      if (row.value.trim()) throw new Error("A property has a value but no name.");
      continue;
    }
    if (key in props) throw new Error(`The property "${key}" appears twice.`);
    switch (row.type) {
      case "text":
        props[key] = row.value;
        break;
      case "number": {
        const number = Number(row.value);
        if (row.value.trim() === "" || !Number.isFinite(number)) throw new Error(`"${key}" is not a number.`);
        props[key] = number;
        break;
      }
      case "boolean":
        props[key] = row.value === "true";
        break;
      case "json":
        try {
          props[key] = JSON.parse(row.value);
        } catch {
          throw new Error(`"${key}" is not valid JSON (lists look like ["a", "b"]).`);
        }
        break;
    }
  }
  return props;
}

/** Key, type and value per property. */
export function PropsEditor({ rows, onChange }: { rows: PropRow[]; onChange: (rows: PropRow[]) => void }) {
  const update = (index: number, patch: Partial<PropRow>) =>
    onChange(rows.map((row, i) => (i === index ? { ...row, ...patch } : row)));
  return (
    <div className="props-editor">
      {rows.map((row, index) => (
        <div key={index} className="prop-row">
          <input
            className="mono"
            placeholder="name"
            value={row.key}
            onChange={(event) => update(index, { key: event.target.value })}
            aria-label="Property name"
          />
          <select
            value={row.type}
            onChange={(event) => {
              const type = event.target.value as PropType;
              update(index, { type, value: type === "boolean" ? (row.value === "true" ? "true" : "false") : row.value });
            }}
            aria-label="Type"
          >
            <option value="text">text</option>
            <option value="number">number</option>
            <option value="boolean">yes/no</option>
            <option value="json">JSON</option>
          </select>
          {row.type === "boolean" ? (
            <select value={row.value} onChange={(event) => update(index, { value: event.target.value })} aria-label="Value">
              <option value="true">true</option>
              <option value="false">false</option>
            </select>
          ) : (
            <input
              className={row.type === "text" ? "" : "mono"}
              placeholder="value"
              value={row.value}
              inputMode={row.type === "number" ? "decimal" : undefined}
              onChange={(event) => update(index, { value: event.target.value })}
              aria-label="Value"
            />
          )}
          <button type="button" className="ghost icon-button" onClick={() => onChange(rows.filter((_, i) => i !== index))} aria-label="Remove property">
            <Icon name="trash" />
          </button>
        </div>
      ))}
      <button type="button" className="ghost small-button" onClick={() => onChange([...rows, { key: "", type: "text", value: "" }])}>
        <Icon name="plus" size={14} /> Add property
      </button>
    </div>
  );
}

export interface Picked {
  id: string;
  name: string;
  labels?: string[];
}

const UUID = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

/** Finds nodes by name, text or id. */
export function useNodeSearch(query: string, limit = 8): { results: Node[]; searching: boolean } {
  const { client, namespace, revision } = useStudio();
  const [results, setResults] = useState<Node[]>([]);
  const [searching, setSearching] = useState(false);
  useEffect(() => {
    const text = query.trim();
    if (!text) {
      setResults([]);
      setSearching(false);
      return;
    }
    let cancelled = false;
    setSearching(true);
    const timer = window.setTimeout(async () => {
      const graph = client.namespace(namespace);
      try {
        const found = new Map<string, Node>();
        if (UUID.test(text)) {
          const node = await graph.getNode(text).catch(() => null);
          if (node) found.set(node.id, node);
        }
        const byName = await graph.query(
          `MATCH (n) WHERE toLower(coalesce(n.name, n.title, n.label, '')) CONTAINS toLower($q) RETURN n LIMIT ${limit}`,
          { q: text },
        );
        for (const [node] of byName.rows as [Node][]) found.set(node.id, node);
        if (found.size < limit) {
          const hits = await graph.searchText(text, limit);
          const nodes = await Promise.all(hits.filter((hit) => !found.has(hit.node)).map((hit) => graph.getNode(hit.node)));
          for (const node of nodes) found.set(node.id, node);
        }
        if (!cancelled) setResults([...found.values()].slice(0, limit));
      } catch {
        if (!cancelled) setResults([]);
      } finally {
        if (!cancelled) setSearching(false);
      }
    }, 180);
    return () => {
      cancelled = true;
      window.clearTimeout(timer);
    };
  }, [client, namespace, query, limit, revision]);
  return { results, searching };
}

/** A field that searches nodes and keeps the chosen one. */
export function NodePicker({
  value,
  onChange,
  placeholder = "Search by name, text or id…",
  autoFocus,
}: {
  value: Picked | null;
  onChange: (node: Picked | null) => void;
  placeholder?: string;
  autoFocus?: boolean;
}) {
  const [query, setQuery] = useState("");
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(0);
  const { results, searching } = useNodeSearch(query);
  const input = useRef<HTMLInputElement>(null);

  if (value) {
    return (
      <div className="picked">
        <span className="dot" style={{ background: labelColor(value.labels?.[0] ?? "") }} />
        <span className="grow-text">{value.name}</span>
        <span className="muted mono small">{value.id.slice(-8)}</span>
        <button type="button"
          className="ghost icon-button"
          onClick={() => {
            onChange(null);
            setTimeout(() => input.current?.focus());
          }}
          aria-label="Change"
        >
          <Icon name="x" />
        </button>
      </div>
    );
  }
  const choose = (node: Node) => {
    onChange({ id: node.id, name: displayName(node), labels: node.labels });
    setQuery("");
    setOpen(false);
  };
  return (
    <div className="picker">
      <input
        ref={input}
        autoFocus={autoFocus}
        value={query}
        placeholder={placeholder}
        onChange={(event) => {
          setQuery(event.target.value);
          setOpen(true);
          setActive(0);
        }}
        onFocus={() => setOpen(true)}
        onBlur={() => setTimeout(() => setOpen(false), 150)}
        onKeyDown={(event) => {
          if (event.key === "ArrowDown") setActive((i) => Math.min(i + 1, results.length - 1));
          else if (event.key === "ArrowUp") setActive((i) => Math.max(i - 1, 0));
          else if (event.key === "Enter" && results[active]) {
            event.preventDefault();
            choose(results[active]);
          }
        }}
      />
      {open && query.trim() && (
        <div className="picker-menu">
          {results.length === 0 && <div className="muted small picker-empty">{searching ? "Searching…" : "No nodes found."}</div>}
          {results.map((node, index) => (
            <button type="button"
              key={node.id}
              className={index === active ? "picker-item active" : "picker-item"}
              onMouseDown={(event) => event.preventDefault()}
              onClick={() => choose(node)}
            >
              <span className="dot" style={{ background: labelColor(node.labels[0] ?? "") }} />
              <span className="grow-text">{displayName(node)}</span>
              <span className="muted small">{node.labels.join(", ")}</span>
            </button>
          ))}
        </div>
      )}
    </div>
  );
}

/** An empty state with the honeycomb behind it. */
export function Empty({ title, children, action }: { title: string; children?: ReactNode; action?: ReactNode }) {
  return (
    <div className="empty">
      <div className="empty-art" aria-hidden="true" />
      <h3>{title}</h3>
      {children && <p className="muted">{children}</p>}
      {action}
    </div>
  );
}

/** Runs async work and reports failures as a toast. */
export function useTask() {
  const { toast } = useStudio();
  const [busy, setBusy] = useState(false);
  const run = async (work: () => Promise<unknown>) => {
    setBusy(true);
    try {
      await work();
      return true;
    } catch (problem) {
      toast(describeError(problem), "error");
      return false;
    } finally {
      setBusy(false);
    }
  };
  return { busy, run };
}
