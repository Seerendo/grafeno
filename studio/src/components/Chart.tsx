// Charts over query results: bars or lines, one shared Y axis (never two),
// categorical colors in a fixed order, a legend for two or more series and a
// tooltip on hover. The table view stays one click away.

import { useEffect, useMemo, useRef, useState } from "react";

import { displayName } from "../api.ts";

type Kind = "bar" | "line";

const MAX_SERIES = 8;
const MAX_POINTS = 500;
const ISO = /^\d{4}-\d{2}-\d{2}(T[\d:.]+Z)?$/;

function isNumber(value: unknown): value is number {
  return typeof value === "number" && Number.isFinite(value);
}

/** A short label for an X value: dates without a midnight time, nodes by name. */
function label(value: unknown): string {
  if (value === null || value === undefined) return "null";
  if (typeof value === "string") return ISO.test(value) ? value.replace(/T00:00:00Z$/, "") : value;
  if (typeof value === "object" && "labels" in (value as object)) {
    return displayName(value as { id: string; labels: string[]; props: Record<string, unknown> });
  }
  if (typeof value === "object") return JSON.stringify(value);
  return String(value);
}

/** Columns whose non-null values are all numbers. */
function numericColumns(columns: string[], rows: unknown[][]): number[] {
  return columns
    .map((_, index) => index)
    .filter((index) => {
      const values = rows.map((row) => row[index]).filter((value) => value !== null && value !== undefined);
      return values.length > 0 && values.every(isNumber);
    });
}

/** Whether the rows can be charted: one numeric column at least, not too many points. */
export function canChart(columns: string[], rows: unknown[][]): boolean {
  return rows.length > 0 && rows.length <= MAX_POINTS && numericColumns(columns, rows).length > 0;
}

/** 0, 1, 2, 2.5, 5 × 10ⁿ steps, about `count` ticks from min to max. */
function ticks(min: number, max: number, count = 5): number[] {
  if (min === max) {
    max = min + 1;
  }
  const raw = (max - min) / count;
  const power = 10 ** Math.floor(Math.log10(raw));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * power).find((s) => s >= raw) ?? raw;
  const out: number[] = [];
  for (let value = Math.floor(min / step) * step; value <= max + step * 1e-9; value += step) {
    out.push(Number(value.toPrecision(12)));
  }
  return out;
}

function formatNumber(value: number): string {
  const abs = Math.abs(value);
  if (abs >= 1e9) return `${(value / 1e9).toFixed(abs >= 1e10 ? 0 : 1)}B`;
  if (abs >= 1e6) return `${(value / 1e6).toFixed(abs >= 1e7 ? 0 : 1)}M`;
  if (abs >= 1e4) return `${(value / 1e3).toFixed(0)}K`;
  if (Number.isInteger(value)) return value.toLocaleString("en-US");
  return value.toLocaleString("en-US", { maximumFractionDigits: 2 });
}

interface Hover {
  index: number;
  x: number;
  y: number;
}

export function Chart({ columns, rows }: { columns: string[]; rows: unknown[][] }) {
  const numeric = useMemo(() => numericColumns(columns, rows), [columns, rows]);
  const firstText = columns.findIndex((_, index) => !numeric.includes(index));
  const [xColumn, setXColumn] = useState(firstText >= 0 ? firstText : 0);
  const [series, setSeries] = useState<number[]>(() =>
    numeric.filter((index) => index !== (firstText >= 0 ? firstText : 0)).slice(0, MAX_SERIES),
  );
  const timeLike = rows.every((row) => typeof row[xColumn] === "string" && ISO.test(row[xColumn] as string));
  const [kind, setKind] = useState<Kind>(timeLike ? "line" : "bar");
  const [width, setWidth] = useState(640);
  const [hover, setHover] = useState<Hover | null>(null);
  const box = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!box.current) return;
    const observer = new ResizeObserver(([entry]) => setWidth(Math.max(320, entry.contentRect.width)));
    observer.observe(box.current);
    return () => observer.disconnect();
  }, []);

  const shown = series.filter((index) => index !== xColumn && numeric.includes(index));
  const values = rows.flatMap((row) => shown.map((index) => row[index]).filter(isNumber));
  const height = 300;
  const margin = { top: 16, right: 16, bottom: 44, left: 56 };
  const plotWidth = width - margin.left - margin.right;
  const plotHeight = height - margin.top - margin.bottom;
  // Bars grow from zero; lines may start above it.
  const low = kind === "bar" ? Math.min(0, ...values) : Math.min(...values);
  const high = kind === "bar" ? Math.max(0, ...values) : Math.max(...values);
  const yTicks = values.length ? ticks(low, high) : [0, 1];
  const yMin = yTicks[0];
  const yMax = yTicks[yTicks.length - 1];
  const y = (value: number) => margin.top + plotHeight - ((value - yMin) / (yMax - yMin || 1)) * plotHeight;
  const band = plotWidth / Math.max(rows.length, 1);
  const xCenter = (index: number) => margin.left + band * (index + 0.5);
  // Thin bars: at most 24px each, with a 2px gap between neighbors.
  const barWidth = Math.max(2, Math.min(24, (band * 0.8 - 2 * (shown.length - 1)) / Math.max(shown.length, 1)));
  const group = shown.length * barWidth + 2 * (shown.length - 1);
  const labelEvery = Math.max(1, Math.ceil(rows.length / Math.max(1, Math.floor(plotWidth / 70))));
  const zero = y(Math.max(yMin, Math.min(0, yMax)));

  const toggle = (index: number) =>
    setSeries((current) =>
      current.includes(index) ? current.filter((item) => item !== index) : [...current, index].slice(-MAX_SERIES),
    );

  const pointer = (event: React.PointerEvent<SVGSVGElement>) => {
    const rect = event.currentTarget.getBoundingClientRect();
    const px = ((event.clientX - rect.left) / rect.width) * width;
    const index = Math.floor((px - margin.left) / band);
    if (index < 0 || index >= rows.length) {
      setHover(null);
      return;
    }
    setHover({ index, x: event.clientX - rect.left, y: event.clientY - rect.top });
  };

  if (numeric.length === 0) {
    return <div className="muted pad small">Nothing to chart: the result has no numeric column.</div>;
  }

  return (
    <div className="chart">
      <div className="chart-controls">
        <div className="segmented">
          <button className={kind === "bar" ? "active" : ""} onClick={() => setKind("bar")}>Bars</button>
          <button className={kind === "line" ? "active" : ""} onClick={() => setKind("line")}>Lines</button>
        </div>
        <label className="row small nowrap">
          <span className="muted">X</span>
          <select value={xColumn} onChange={(event) => setXColumn(Number(event.target.value))}>
            {columns.map((column, index) => (
              <option key={column} value={index}>{column}</option>
            ))}
          </select>
        </label>
        <span className="muted small">Y</span>
        {numeric
          .filter((index) => index !== xColumn)
          .map((index) => {
            const slot = shown.indexOf(index);
            return (
              <button
                key={index}
                className={slot >= 0 ? "chip active series-chip" : "chip series-chip"}
                onClick={() => toggle(index)}
                title={slot >= 0 ? "Hide this series" : "Show this series"}
              >
                {slot >= 0 && <span className="dot" style={{ background: `var(--series-${slot + 1})` }} />}
                {columns[index]}
              </button>
            );
          })}
      </div>

      <div className="chart-plot" ref={box}>
        {shown.length === 0 ? (
          <div className="muted pad small">Choose a column for Y.</div>
        ) : (
          <svg
            width={width}
            height={height}
            viewBox={`0 0 ${width} ${height}`}
            onPointerMove={pointer}
            onPointerLeave={() => setHover(null)}
            role="img"
            aria-label={`${kind === "bar" ? "Bar" : "Line"} chart of ${shown.map((index) => columns[index]).join(", ")} by ${columns[xColumn]}`}
          >
            {yTicks.map((tick) => (
              <g key={tick}>
                <line className="chart-grid" x1={margin.left} x2={width - margin.right} y1={y(tick)} y2={y(tick)} />
                <text className="chart-tick" x={margin.left - 8} y={y(tick)} textAnchor="end" dominantBaseline="middle">
                  {formatNumber(tick)}
                </text>
              </g>
            ))}
            <line className="chart-axis" x1={margin.left} x2={width - margin.right} y1={zero} y2={zero} />
            {rows.map((row, index) =>
              index % labelEvery === 0 ? (
                <text key={index} className="chart-tick" x={xCenter(index)} y={height - margin.bottom + 18} textAnchor="middle">
                  {label(row[xColumn]).slice(0, 14)}
                </text>
              ) : null,
            )}
            {hover && kind === "line" && (
              <line className="chart-crosshair" x1={xCenter(hover.index)} x2={xCenter(hover.index)} y1={margin.top} y2={margin.top + plotHeight} />
            )}
            {kind === "bar"
              ? rows.map((row, index) =>
                  shown.map((column, slot) => {
                    const value = row[column];
                    if (!isNumber(value)) return null;
                    const left = xCenter(index) - group / 2 + slot * (barWidth + 2);
                    const top = Math.min(y(value), zero);
                    const size = Math.max(1, Math.abs(y(value) - zero));
                    // 4px rounded at the data end, square at the baseline.
                    const r = Math.min(4, size, barWidth / 2);
                    const up = value >= 0;
                    const path = up
                      ? `M${left},${top + size}V${top + r}Q${left},${top} ${left + r},${top}H${left + barWidth - r}Q${left + barWidth},${top} ${left + barWidth},${top + r}V${top + size}Z`
                      : `M${left},${top}V${top + size - r}Q${left},${top + size} ${left + r},${top + size}H${left + barWidth - r}Q${left + barWidth},${top + size} ${left + barWidth},${top + size - r}V${top}Z`;
                    return (
                      <path
                        key={`${index}-${column}`}
                        d={path}
                        fill={`var(--series-${slot + 1})`}
                        opacity={hover && hover.index !== index ? 0.45 : 1}
                      />
                    );
                  }),
                )
              : shown.map((column, slot) => {
                  const points = rows
                    .map((row, index) => (isNumber(row[column]) ? `${xCenter(index)},${y(row[column] as number)}` : null))
                    .filter(Boolean);
                  return (
                    <g key={column}>
                      <polyline
                        points={points.join(" ")}
                        fill="none"
                        stroke={`var(--series-${slot + 1})`}
                        strokeWidth={2}
                        strokeLinejoin="round"
                        strokeLinecap="round"
                      />
                      {rows.map((row, index) =>
                        isNumber(row[column]) && (rows.length <= 40 || hover?.index === index) ? (
                          <circle
                            key={index}
                            cx={xCenter(index)}
                            cy={y(row[column] as number)}
                            r={4}
                            fill={`var(--series-${slot + 1})`}
                            stroke="var(--panel)"
                            strokeWidth={2}
                          />
                        ) : null,
                      )}
                    </g>
                  );
                })}
          </svg>
        )}
        {hover && shown.length > 0 && (
          <div
            className="chart-tooltip"
            style={{ left: Math.min(hover.x + 14, width - 180), top: Math.max(hover.y - 10, 0) }}
          >
            <div className="muted small">{label(rows[hover.index][xColumn])}</div>
            {shown.map((column, slot) => (
              <div key={column} className="tooltip-row">
                <span className="dot" style={{ background: `var(--series-${slot + 1})` }} />
                <strong>{isNumber(rows[hover.index][column]) ? formatNumber(rows[hover.index][column] as number) : "—"}</strong>
                <span className="muted small">{columns[column]}</span>
              </div>
            ))}
          </div>
        )}
      </div>
      {shown.length >= 2 && (
        <div className="chart-legend small">
          {shown.map((column, slot) => (
            <span key={column} className="row nowrap">
              <span className="dot" style={{ background: `var(--series-${slot + 1})` }} />
              {columns[column]}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}
