import { useEffect, useRef, useState } from "react";
import type { RefObject } from "react";

// Inline SVG drawn at the real pixel width, so the text stays readable (the
// approved mock does the same). One series per chart, in the accent colour;
// every value is also in a hidden table and in each mark's tooltip.

const FALLBACK_WIDTH = 320;

function useWidth(ref: RefObject<HTMLElement | null>, minimum: number): number {
  const [width, setWidth] = useState(FALLBACK_WIDTH);
  useEffect(() => {
    const element = ref.current;
    if (!element) return;
    const measure = () => {
      const measured = Math.floor(element.clientWidth);
      if (measured > 0) setWidth(Math.max(minimum, measured));
    };
    measure();
    if (typeof ResizeObserver === "undefined") return;
    const observer = new ResizeObserver(measure);
    observer.observe(element);
    return () => observer.disconnect();
  }, [ref, minimum]);
  return width;
}

export interface Point {
  // "W44"
  label: string;
  // "Mon 26 Oct"
  start: string;
  value: number | null;
}

// A round top for the axis and the step between its gridlines: the smallest
// step of 1, 2 or 5 (times a power of ten) that fits the data in about three
// intervals.
// 0.6000000000000001 is 0.6 to anyone reading the axis.
const clean = (value: number) => Number(value.toPrecision(10));

export function niceAxis(max: number): { top: number; step: number } {
  if (max <= 0) return { top: 1, step: 1 };
  const raw = max / 3;
  const power = 10 ** Math.floor(Math.log10(raw));
  const step = clean(
    ([1, 2, 5, 10].map((m) => m * power).find((s) => s >= raw) ?? power * 10) as number,
  );
  return { top: clean(Math.ceil(max / step - 1e-9) * step), step };
}

interface ChartProps {
  title: string;
  points: Point[];
  // Rounds a value for the axis, the cap label and the tooltip.
  format: (value: number) => string;
  unit: string;
}

const HEIGHT = 212;
const FRAME = { l: 30, r: 6, t: 14, b: 26 };

function frame(width: number) {
  const pw = width - FRAME.l - FRAME.r;
  const ph = HEIGHT - FRAME.t - FRAME.b;
  return { l: FRAME.l, r: width - FRAME.r, b: HEIGHT - FRAME.b, pw, ph };
}

function Axis({
  f,
  top,
  step,
  format,
}: {
  f: ReturnType<typeof frame>;
  top: number;
  step: number;
  format: (v: number) => string;
}) {
  const ticks = Array.from({ length: Math.round(top / step) + 1 }, (_, i) => i * step);
  return (
    <>
      {ticks.map((value) => {
        const y = f.b - (f.ph * value) / top;
        return (
          <g key={value}>
            <line className="gl" x1={f.l} x2={f.r} y1={y} y2={y} />
            <text x={f.l - 8} y={y + 3.5} textAnchor="end">
              {format(value)}
            </text>
          </g>
        );
      })}
    </>
  );
}

function WeekLabels({ f, points }: { f: ReturnType<typeof frame>; points: Point[] }) {
  const slot = f.pw / points.length;
  return (
    <>
      {points.map((point, index) => (
        <text
          key={point.start}
          x={f.l + slot * index + slot / 2}
          y={f.b + 17}
          textAnchor="middle"
          className={index === points.length - 1 ? "hl" : undefined}
        >
          {point.label}
        </text>
      ))}
    </>
  );
}

// The numbers behind a chart, for readers who skip the picture.
function DataTable({ title, points, format, unit }: ChartProps) {
  return (
    <table className="sr-only">
      <caption>{title}</caption>
      <tbody>
        {points.map((point) => (
          <tr key={point.start}>
            <th scope="row">{`${point.label}, week of ${point.start}`}</th>
            <td>{point.value === null ? "no data" : `${format(point.value)} ${unit}`}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function summary({ title, points, format, unit }: ChartProps): string {
  const last = points.at(-1);
  const now = last?.value == null ? "no data" : `${format(last.value)} ${unit}`;
  return `${title}, ${points[0]?.label ?? ""} to ${last?.label ?? ""}. Latest week: ${now}.`;
}

export function BarChart(props: ChartProps) {
  const ref = useRef<HTMLDivElement>(null);
  const width = useWidth(ref, 240);
  const { points, format, unit } = props;
  const f = frame(width);
  const { top, step } = niceAxis(Math.max(0, ...points.map((p) => p.value ?? 0)));
  const slot = f.pw / Math.max(1, points.length);
  const bar = Math.min(34, slot * 0.58);
  const last = points.length - 1;
  return (
    <div className="chart" ref={ref}>
      <svg
        viewBox={`0 0 ${width} ${HEIGHT}`}
        width={width}
        height={HEIGHT}
        role="img"
        aria-label={summary(props)}
      >
        <Axis f={f} top={top} step={step} format={format} />
        {points.map((point, index) => {
          if (point.value === null) return null;
          const x = f.l + slot * index + (slot - bar) / 2;
          // A zero keeps a 2 px stub so the week is visibly there.
          const h = point.value === 0 ? 2 : (f.ph * point.value) / top;
          return (
            <g key={point.start}>
              <rect
                className="b-acc"
                x={x}
                y={f.b - h}
                width={bar}
                height={h}
                rx={point.value === 0 ? 1 : 3}
              >
                <title>{`${point.label} (${point.start}): ${format(point.value)} ${unit}`}</title>
              </rect>
              <text
                x={x + bar / 2}
                y={f.b - h - 5}
                textAnchor="middle"
                className={index === last ? "hl" : undefined}
              >
                {format(point.value)}
              </text>
            </g>
          );
        })}
        <WeekLabels f={f} points={points} />
      </svg>
      <DataTable {...props} />
    </div>
  );
}

interface StackPart {
  // The class that paints it: "b-cor", "b-esc" or "b-rew".
  className: string;
  label: string;
}

export interface StackPoint {
  label: string;
  start: string;
  // One value per part, bottom up; null when the week has no outcome to
  // divide by.
  values: number[] | null;
}

interface StackProps {
  title: string;
  parts: StackPart[];
  points: StackPoint[];
  format: (value: number) => string;
  unit: string;
}

const total = (point: StackPoint) => point.values?.reduce((sum, value) => sum + value, 0) ?? null;

// A bar per week split into its parts, with a 1 px surface gap between the
// segments and the week's total on top. The legend names the parts, and every
// value is in the hidden table and in each segment's tooltip.
export function StackedBarChart({ title, parts, points, format, unit }: StackProps) {
  const ref = useRef<HTMLDivElement>(null);
  const width = useWidth(ref, 240);
  const f = frame(width);
  const { top, step } = niceAxis(Math.max(0, ...points.map((p) => total(p) ?? 0)));
  const slot = f.pw / Math.max(1, points.length);
  const bar = Math.min(34, slot * 0.58);
  const last = points.length - 1;
  const flat = points.map((point) => ({
    ...point,
    value: total(point),
  }));
  return (
    <div className="chart-stack">
      <div className="legend">
        {parts.map((part) => (
          <span key={part.label}>
            <i className={`sw ${part.className}`} />
            {part.label}
          </span>
        ))}
      </div>
      <div className="chart" ref={ref}>
        <svg
          viewBox={`0 0 ${width} ${HEIGHT}`}
          width={width}
          height={HEIGHT}
          role="img"
          aria-label={summary({ title, points: flat, format, unit })}
        >
          <Axis f={f} top={top} step={step} format={format} />
          {points.map((point, index) => {
            const sum = total(point);
            if (point.values === null || sum === null) return null;
            const x = f.l + slot * index + (slot - bar) / 2;
            let y = f.b;
            const segments = point.values.map((value, part) => {
              const h = (f.ph * value) / top;
              y -= h;
              if (value === 0) return null;
              return (
                <rect
                  key={parts[part]?.label}
                  className={parts[part]?.className}
                  x={x}
                  y={y}
                  width={bar}
                  height={Math.max(h - 1, 0.5)}
                  rx={1.5}
                >
                  <title>{`${point.label} (${point.start}): ${format(value)} ${parts[part]?.label.toLowerCase()}`}</title>
                </rect>
              );
            });
            return (
              <g key={point.start}>
                {sum === 0 ? (
                  <rect className="b-acc" x={x} y={f.b - 2} width={bar} height={2} rx={1} />
                ) : null}
                {segments}
                <text
                  x={x + bar / 2}
                  y={y - 5}
                  textAnchor="middle"
                  className={index === last ? "hl" : undefined}
                >
                  {format(sum)}
                </text>
              </g>
            );
          })}
          <WeekLabels f={f} points={flat} />
        </svg>
        <table className="sr-only">
          <caption>{title}</caption>
          <tbody>
            {points.map((point) => (
              <tr key={point.start}>
                <th scope="row">{`${point.label}, week of ${point.start}`}</th>
                <td>
                  {point.values === null
                    ? "no data"
                    : `${format(total(point) ?? 0)} ${unit}: ${point.values
                        .map(
                          (value, part) => `${format(value)} ${parts[part]?.label.toLowerCase()}`,
                        )
                        .join(", ")}`}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </div>
  );
}

export function LineChart(props: ChartProps) {
  const ref = useRef<HTMLDivElement>(null);
  const width = useWidth(ref, 240);
  const { points, format, unit } = props;
  const f = frame(width);
  const { top, step } = niceAxis(Math.max(0, ...points.map((p) => p.value ?? 0)));
  const slot = f.pw / Math.max(1, points.length);
  const x = (index: number) => f.l + slot * index + slot / 2;
  const y = (value: number) => f.b - (f.ph * value) / top;
  // A week with no data breaks the line instead of dropping it to zero.
  const runs: { index: number; value: number }[][] = [];
  points.forEach((point, index) => {
    if (point.value === null) {
      runs.push([]);
      return;
    }
    if (runs.length === 0) runs.push([]);
    runs[runs.length - 1]?.push({ index, value: point.value });
  });
  const last = points.length - 1;
  const latest = points[last]?.value ?? null;
  return (
    <div className="chart" ref={ref}>
      <svg
        viewBox={`0 0 ${width} ${HEIGHT}`}
        width={width}
        height={HEIGHT}
        role="img"
        aria-label={summary(props)}
      >
        <Axis f={f} top={top} step={step} format={format} />
        {runs
          .filter((run) => run.length > 1)
          .map((run) => {
            const line = run.map((p) => `${x(p.index)},${y(p.value)}`);
            const first = run[0];
            const end = run[run.length - 1];
            if (!first || !end) return null;
            return (
              <g key={first.index}>
                <path
                  className="area"
                  d={`M${x(first.index)},${f.b} L${line.join(" L")} L${x(end.index)},${f.b}z`}
                />
                <polyline className="ln" points={line.join(" ")} />
              </g>
            );
          })}
        {points.map((point, index) =>
          point.value === null ? null : (
            <circle
              key={point.start}
              className={`pt${index === last ? " cur" : ""}`}
              cx={x(index)}
              cy={y(point.value)}
              r={index === last ? 4.5 : 3.5}
            >
              <title>{`${point.label} (${point.start}): ${format(point.value)} ${unit}`}</title>
            </circle>
          ),
        )}
        {latest === null ? null : (
          <text className="hl" x={x(last)} y={y(latest) + 20} textAnchor="end">
            {`${format(latest)} ${unit}`}
          </text>
        )}
        <WeekLabels f={f} points={points} />
      </svg>
      <DataTable {...props} />
    </div>
  );
}

// The 8-week trend under a signal: a line with a dot per week, the newest
// larger, and the first and last week named.
export function Spark({
  label,
  values,
  weeks,
}: {
  label: string;
  values: number[];
  weeks: { label: string; start: string }[];
}) {
  const ref = useRef<HTMLDivElement>(null);
  const width = useWidth(ref, 140);
  const h = 64;
  const pad = 6;
  const top = 6;
  const bottom = 16;
  const lo = Math.min(...values);
  const span = Math.max(...values) - lo || 1;
  const x = (i: number) => pad + ((width - 2 * pad) * i) / Math.max(1, values.length - 1);
  const y = (v: number) => top + (h - top - bottom) * (1 - (v - lo) / span);
  const last = values.length - 1;
  return (
    <div className="spark" ref={ref}>
      <svg
        viewBox={`0 0 ${width} ${h}`}
        width={width}
        height={h}
        role="img"
        aria-label={`${label}, last ${values.length} weeks: ${values.join(", ")}`}
      >
        <line
          className="sp-axis"
          x1={pad}
          x2={width - pad}
          y1={h - bottom + 2}
          y2={h - bottom + 2}
        />
        <polyline className="sp-now" points={values.map((v, i) => `${x(i)},${y(v)}`).join(" ")} />
        {values.map((v, i) => (
          <circle
            key={weeks[i]?.start ?? i}
            className={`sp-pt${i === last ? " cur" : ""}`}
            cx={x(i)}
            cy={y(v)}
            r={i === last ? 4 : 2.5}
          >
            <title>{`${weeks[i]?.label ?? ""}: ${v}`}</title>
          </circle>
        ))}
        <text x={pad} y={h - 2} textAnchor="start">
          {weeks[0]?.label}
        </text>
        <text x={width - pad} y={h - 2} textAnchor="end" className="hl">
          {weeks[last]?.label}
        </text>
      </svg>
    </div>
  );
}
