import { displayName, go, labelColor } from "../api.ts";

type Json = unknown;

function isNode(value: Json): value is { id: string; labels: string[]; props: Record<string, unknown>; created_at: number } {
  return typeof value === "object" && value !== null && "labels" in value && "created_at" in value;
}

function isEdge(value: Json): value is { id: string; label: string; from: string; to: string; recorded_at: number } {
  return typeof value === "object" && value !== null && "from" in value && "recorded_at" in value;
}

/** One value of a query result: nodes link to the explorer. */
export function Cell({ value }: { value: Json }) {
  if (value === null || value === undefined) return <span className="muted">null</span>;
  if (isNode(value)) {
    return (
      <a href={`#/explorer/${value.id}`} title={value.id}>
        <span className="dot" style={{ background: labelColor(value.labels[0] ?? "") }} /> {displayName(value)}{" "}
        <span className="muted small">:{value.labels.join(":")}</span>
      </a>
    );
  }
  if (isEdge(value)) return <span className="mono">[:{value.label}]</span>;
  if (Array.isArray(value)) {
    return (
      <span>
        [
        {value.map((item, index) => (
          <span key={index}>
            {index > 0 && ", "}
            <Cell value={item} />
          </span>
        ))}
        ]
      </span>
    );
  }
  if (typeof value === "object") return <span className="mono">{JSON.stringify(value)}</span>;
  return <span>{String(value)}</span>;
}

export function ResultTable({ columns, rows }: { columns: string[]; rows: Json[][] }) {
  if (columns.length === 0) return null;
  return (
    <div className="table-wrap">
      <table>
        <thead>
          <tr>
            {columns.map((column) => (
              <th key={column}>{column}</th>
            ))}
          </tr>
        </thead>
        <tbody>
          {rows.map((row, index) => (
            <tr key={index}>
              {row.map((value, column) => (
                <td key={column}>
                  <Cell value={value} />
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

export function openNode(id: string): void {
  go("explorer", id);
}
