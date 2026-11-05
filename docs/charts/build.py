"""Draws the charts of docs/performance.md from the CSV files in docs/data.

    python docs/charts/build.py

Writes docs/assets/charts/<name>-light.svg and <name>-dark.svg. Plain
Python, no packages: every chart is SVG drawn here, with the colors of a
validated categorical palette (light and dark steps) and thin marks. Run it
again after changing a CSV.
"""
import csv
import math
import os

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), '..', '..'))
DATA = os.path.join(ROOT, 'docs', 'data')
OUT = os.path.join(ROOT, 'docs', 'assets', 'charts')

THEMES = {
    'light': {
        'surface': '#fcfcfb', 'text': '#0b0b0b', 'secondary': '#52514e', 'muted': '#76756f',
        'grid': '#e4e3df', 'axis': '#c9c8c2', 'gray': '#b4b3ad',
        'series': ['#2a78d6', '#eb6834', '#1baf7a', '#eda100', '#e87ba4', '#008300', '#6250d6'],
    },
    'dark': {
        'surface': '#1a1a19', 'text': '#f0efec', 'secondary': '#c3c2b7', 'muted': '#9a998f',
        'grid': '#2e2e2c', 'axis': '#45453f', 'gray': '#5c5c56',
        'series': ['#3987e5', '#d95926', '#199e70', '#c98500', '#d55181', '#008300', '#9085e9'],
    },
}
FONT = "system-ui, -apple-system, 'Segoe UI', Roboto, 'Helvetica Neue', Arial, sans-serif"


def read(name):
    with open(os.path.join(DATA, name), encoding='utf-8') as f:
        return list(csv.DictReader(f))


def num(value):
    return None if value in ('', None) else float(value)


def esc(text):
    return str(text).replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;')


def text_width(text, size=12):
    return len(str(text)) * size * 0.56


def fmt(value, unit=''):
    if value is None:
        return ''
    a = abs(value)
    if a >= 10000:
        s = f'{value / 1000:.0f}k' if a < 1e6 else f'{value / 1e6:.1f}M'
    elif a >= 100:
        s = f'{value:,.0f}'.replace(',', ' ')
    elif a >= 10:
        s = f'{value:.0f}' if value == int(value) else f'{value:.1f}'
    elif a >= 1:
        s = f'{value:.1f}'.rstrip('0').rstrip('.')
    elif a == 0:
        s = '0'
    else:
        s = f'{value:.2g}'
    return s + unit


def tick_label(value, unit=''):
    if value >= 1000 and value % 1000 == 0:
        k = value / 1000
        return (f'{k:.0f}k' if k < 1000 else f'{k / 1000:.0f}M') + unit
    return fmt(value, unit)


class Svg:
    def __init__(self, width, height, theme, title, desc):
        self.w, self.h, self.t = width, height, THEMES[theme]
        self.parts = []
        self.title, self.desc = title, desc

    def add(self, s):
        self.parts.append(s)

    def text(self, x, y, s, size=12, color='secondary', anchor='start', weight=400):
        self.add(
            f'<text x="{x:.1f}" y="{y:.1f}" font-size="{size}" fill="{self.t[color]}" '
            f'text-anchor="{anchor}" font-weight="{weight}">{esc(s)}</text>'
        )

    def line(self, x1, y1, x2, y2, color='grid', width=1):
        self.add(
            f'<line x1="{x1:.1f}" y1="{y1:.1f}" x2="{x2:.1f}" y2="{y2:.1f}" '
            f'stroke="{self.t[color] if color in self.t else color}" stroke-width="{width}"/>'
        )

    def bar_h(self, x0, y, length, thick, color):
        """A horizontal bar from x0, square at the baseline, 4px round tip."""
        if length <= 0:
            return
        r = min(4, length, thick / 2)
        x1 = x0 + length
        self.add(
            f'<path d="M{x0:.1f},{y:.1f} H{x1 - r:.1f} Q{x1:.1f},{y:.1f} {x1:.1f},{y + r:.1f} '
            f'V{y + thick - r:.1f} Q{x1:.1f},{y + thick:.1f} {x1 - r:.1f},{y + thick:.1f} '
            f'H{x0:.1f} Z" fill="{color}"/>'
        )

    def bar_v(self, x, base, height, thick, color):
        if height <= 0:
            return
        r = min(4, height, thick / 2)
        top = base - height
        self.add(
            f'<path d="M{x:.1f},{base:.1f} V{top + r:.1f} Q{x:.1f},{top:.1f} {x + r:.1f},{top:.1f} '
            f'H{x + thick - r:.1f} Q{x + thick:.1f},{top:.1f} {x + thick:.1f},{top + r:.1f} '
            f'V{base:.1f} Z" fill="{color}"/>'
        )

    def dot(self, x, y, color, r=4):
        self.add(
            f'<circle cx="{x:.1f}" cy="{y:.1f}" r="{r}" fill="{color}" '
            f'stroke="{self.t["surface"]}" stroke-width="2"/>'
        )

    def polyline(self, points, color):
        d = ' '.join(f'{x:.1f},{y:.1f}' for x, y in points)
        self.add(
            f'<polyline points="{d}" fill="none" stroke="{color}" stroke-width="2" '
            f'stroke-linejoin="round" stroke-linecap="round"/>'
        )

    def legend(self, x, y, items):
        """Items: (label, color, kind) with kind 'bar' or 'line'."""
        for label, color, kind in items:
            if kind == 'line':
                self.add(f'<line x1="{x}" y1="{y - 4}" x2="{x + 16}" y2="{y - 4}" stroke="{color}" stroke-width="2" stroke-linecap="round"/>')
                self.dot(x + 8, y - 4, color, 3)
                x += 22
            else:
                self.add(f'<rect x="{x}" y="{y - 9}" width="10" height="10" rx="2" fill="{color}"/>')
                x += 15
            self.text(x, y, label, 12, 'secondary')
            x += text_width(label) + 18

    def render(self):
        head = (
            f'<svg xmlns="http://www.w3.org/2000/svg" width="{self.w}" height="{self.h}" '
            f'viewBox="0 0 {self.w} {self.h}" role="img" font-family="{FONT}">'
            f'<title>{esc(self.title)}</title><desc>{esc(self.desc)}</desc>'
            f'<rect width="100%" height="100%" rx="8" fill="{self.t["surface"]}"/>'
        )
        return head + ''.join(self.parts) + '</svg>\n'


def header(svg, title, subtitle):
    svg.text(24, 34, title, 16, 'text', weight=600)
    if subtitle:
        svg.text(24, 54, subtitle, 12, 'muted')


# ---------------------------------------------------------------- scales

def log_ticks(lo, hi):
    ticks = []
    e = math.floor(math.log10(lo))
    while 10 ** e <= hi * 1.0001:
        for m in (1, 2, 5) if hi / lo < 1000 else (1,):
            v = m * 10 ** e
            if lo * 0.9999 <= v <= hi * 1.0001:
                ticks.append(v)
        e += 1
    return ticks


def nice_ticks(lo, hi, n=5):
    span = hi - lo
    raw = span / n
    mag = 10 ** math.floor(math.log10(raw))
    step = min((m * mag for m in (1, 2, 2.5, 5, 10) if m * mag >= raw), default=mag * 10)
    start = math.floor(lo / step) * step
    ticks = []
    v = start
    while True:
        ticks.append(round(v, 10))
        if v >= hi - step * 0.001:
            break
        v += step
    return ticks


class Scale:
    def __init__(self, lo, hi, a, b, log=False):
        self.lo, self.hi, self.a, self.b, self.log = lo, hi, a, b, log

    def __call__(self, v):
        if self.log:
            v = max(v, self.lo)
            f = (math.log10(v) - math.log10(self.lo)) / (math.log10(self.hi) - math.log10(self.lo))
        else:
            f = (v - self.lo) / (self.hi - self.lo)
        return self.a + f * (self.b - self.a)

    def ticks(self):
        return log_ticks(self.lo, self.hi) if self.log else nice_ticks(self.lo, self.hi)


def log_bounds(values):
    """The 1-2-5 values just around the data."""
    vs = [v for v in values if v and v > 0]
    lo_v, hi_v = min(vs), max(vs)
    steps = [m * 10 ** e for e in range(-4, 9) for m in (1, 2, 5)]
    lo = max(s for s in steps if s <= lo_v)
    hi = min(s for s in steps if s >= hi_v)
    return lo, hi


# ---------------------------------------------------------------- charts

def hbars(name, title, subtitle, rows, unit, log=False, series=None, note=None, limit=None, legend=None, top_value=None):
    """rows: (label, [values per series], highlight) ; series: legend labels."""
    series = series or [None]
    ns = len(series)
    label_w = max(text_width(r[0]) for r in rows) + 16
    thick = 14 if ns > 1 else 16
    band = thick * ns + 2 * (ns - 1) + 14
    top = 72 + (22 if ns > 1 or legend else 0)
    width = 760
    plot_x, plot_r = 24 + label_w, width - 70
    height = top + band * len(rows) + 44 + (18 if note else 0)
    values = [v for r in rows for v in r[1] if v is not None] + ([limit[0]] if limit else [])
    if log:
        lo, hi = log_bounds(values)
        # Bars need room below the smallest value, or it draws nothing.
        smallest = min(v for v in values if v > 0)
        steps = [m * 10 ** e for e in range(-4, 9) for m in (1, 2, 5)]
        lo = max(s for s in steps if s <= smallest * 0.5)
    elif top_value:
        lo, hi = 0, top_value
    else:
        lo, hi = 0, max(values) * 1.08
        t = nice_ticks(0, hi)
        hi = t[-1]
    for theme in THEMES:
        svg = Svg(width, height, theme, title, subtitle)
        header(svg, title, subtitle)
        if ns > 1:
            svg.legend(24, 80, [(s, svg.t['series'][i], 'bar') for i, s in enumerate(series)])
        if legend:
            svg.legend(24, 80, [(label, svg.t['series'][slot], 'bar') for label, slot in legend])
        sx = Scale(lo, hi, plot_x, plot_r, log)
        base_y = top + band * len(rows)
        for tv in sx.ticks():
            x = sx(tv)
            svg.line(x, top - 6, x, base_y, 'grid')
            svg.text(x, base_y + 16, tick_label(tv, unit), 11, 'muted', 'middle')
        if limit:
            x = sx(limit[0])
            svg.line(x, top - 10, x, base_y, svg.t['secondary'], 1)
            svg.text(x - 4, top - 12, limit[1], 11, 'secondary', 'end')
        for i, (label, vals, highlight) in enumerate(rows):
            y = top + i * band + 7
            svg.text(24 + label_w - 12, y + (thick * ns + 2 * (ns - 1)) / 2 + 4, label, 12, 'text', 'end')
            for k, v in enumerate(vals):
                yy = y + k * (thick + 2)
                if v is None:
                    svg.text(plot_x + 6, yy + thick - 3, 'not measured', 11, 'muted')
                    continue
                if ns > 1:
                    color = svg.t['series'][k]
                else:
                    color = svg.t['series'][0] if highlight in (None, True) else svg.t['gray']
                    if isinstance(highlight, int) and not isinstance(highlight, bool):
                        color = svg.t['series'][highlight]
                x0 = sx(lo) if log else sx(0)
                svg.bar_h(x0, yy, sx(v) - x0, thick, color)
                svg.text(sx(v) + 6, yy + thick - 3, fmt(v, unit), 11, 'secondary')
        svg.line(plot_x, top - 6, plot_x, base_y, 'axis')
        if note:
            svg.text(24, height - 14, note, 11, 'muted')
        write(name, theme, svg)


def lines(name, title, subtitle, series, xlabel, ylabel, logx=True, logy=True, note=None, ylim=None, xunit='', yunit=''):
    """series: (label, [(x, y)])."""
    width, height = 760, 420 + (18 if note else 0)
    left, right, top, bottom = 78, 150, 96, 360
    xs = [x for _, pts in series for x, _ in pts]
    ys = [y for _, pts in series for _, y in pts]
    xlo, xhi = log_bounds(xs) if logx else (min(xs), max(xs))
    if ylim:
        ylo, yhi = ylim
    else:
        ylo, yhi = log_bounds(ys) if logy else (0, nice_ticks(0, max(ys))[-1])
    for theme in THEMES:
        svg = Svg(width, height, theme, title, subtitle)
        header(svg, title, subtitle)
        svg.legend(24, 80, [(s, svg.t['series'][i], 'line') for i, (s, _) in enumerate(series)])
        sx = Scale(xlo, xhi, left, width - right, logx)
        sy = Scale(ylo, yhi, bottom, top, logy)
        for tv in sy.ticks():
            y = sy(tv)
            svg.line(left, y, width - right, y, 'grid')
            svg.text(left - 8, y + 4, tick_label(tv, yunit), 11, 'muted', 'end')
        for tv in sx.ticks():
            x = sx(tv)
            svg.line(x, bottom, x, bottom + 4, 'axis')
            svg.text(x, bottom + 18, tick_label(tv, xunit), 11, 'muted', 'middle')
        svg.line(left, bottom, width - right, bottom, 'axis')
        svg.text((left + width - right) / 2, bottom + 40, xlabel, 12, 'secondary', 'middle')
        svg.add(
            f'<text transform="translate(20,{(top + bottom) / 2}) rotate(-90)" font-size="12" '
            f'fill="{svg.t["secondary"]}" text-anchor="middle">{esc(ylabel)}</text>'
        )
        ends = []
        for i, (label, pts) in enumerate(series):
            color = svg.t['series'][i]
            p = [(sx(x), sy(y)) for x, y in pts]
            svg.polyline(p, color)
            for x, y in p:
                svg.dot(x, y, color, 3)
            ends.append([p[-1][1], label, p[-1][0], pts[-1][1]])
        # End labels, spread so they do not overlap, with a leader line.
        ends.sort()
        for k in range(1, len(ends)):
            if ends[k][0] - ends[k - 1][0] < 15:
                ends[k][0] = ends[k - 1][0] + 15
        for y, label, x, value in ends:
            lx = width - right + 12
            svg.line(x + 5, min(max(y, top), bottom), lx - 2, min(max(y, top), bottom), 'axis')
            svg.text(lx, min(max(y, top), bottom) + 4, f'{fmt(value, yunit)} {label}' if len(label) < 14 else fmt(value, yunit), 11, 'secondary')
        if note:
            svg.text(24, height - 14, note, 11, 'muted')
        write(name, theme, svg)


def columns(name, title, subtitle, cats, values, unit, xlabel, note=None):
    width, height = 760, 380 + (18 if note else 0)
    left, right, top, bottom = 64, 24, 80, 320
    hi = nice_ticks(0, max(values) * 1.08)[-1]
    n = len(cats)
    band = (width - left - right) / n
    thick = min(24, band * 0.6)
    for theme in THEMES:
        svg = Svg(width, height, theme, title, subtitle)
        header(svg, title, subtitle)
        sy = Scale(0, hi, bottom, top)
        for tv in sy.ticks():
            y = sy(tv)
            svg.line(left, y, width - right, y, 'grid')
            svg.text(left - 8, y + 4, tick_label(tv, unit), 11, 'muted', 'end')
        for i, (c, v) in enumerate(zip(cats, values)):
            x = left + i * band + (band - thick) / 2
            svg.bar_v(x, bottom, bottom - sy(v), thick, svg.t['series'][0])
            if i in (0, n - 1) or v == max(values):
                svg.text(x + thick / 2, sy(v) - 6, fmt(v, unit), 11, 'secondary', 'middle')
            svg.text(x + thick / 2, bottom + 16, c, 11, 'muted', 'middle')
        svg.line(left, bottom, width - right, bottom, 'axis')
        svg.text((left + width - right) / 2, bottom + 38, xlabel, 12, 'secondary', 'middle')
        if note:
            svg.text(24, height - 14, note, 11, 'muted')
        write(name, theme, svg)


def dots(name, title, subtitle, rows, series, lo, hi, unit=''):
    """rows: (label, [value per series]): a dot plot on one axis."""
    label_w = max(text_width(r[0]) for r in rows) + 16
    width = 760
    top, band = 100, 30
    plot_x, plot_r = 24 + label_w, width - 40
    height = top + band * len(rows) + 40
    for theme in THEMES:
        svg = Svg(width, height, theme, title, subtitle)
        header(svg, title, subtitle)
        svg.legend(24, 80, [(s, svg.t['series'][i], 'dot') for i, s in enumerate(series)])
        sx = Scale(lo, hi, plot_x, plot_r)
        base = top + band * len(rows)
        for tv in nice_ticks(lo, hi):
            if lo - 1e-9 <= tv <= hi + 1e-9:
                x = sx(tv)
                svg.line(x, top - 8, x, base, 'grid')
                svg.text(x, base + 16, f'{tv:.2f}', 11, 'muted', 'middle')
        for i, (label, vals) in enumerate(rows):
            y = top + i * band + band / 2
            svg.text(plot_x - 12, y + 4, label, 12, 'text', 'end')
            svg.line(plot_x, y, plot_r, y, 'grid')
            present = [v for v in vals if v is not None]
            if len(present) == 2:
                svg.line(sx(min(present)), y, sx(max(present)), y, 'axis', 2)
            # The first series last, on top where they meet.
            for k in reversed(range(len(vals))):
                if vals[k] is not None:
                    svg.dot(sx(vals[k]), y, svg.t['series'][k], 5)
        write(name, theme, svg)


def write(name, theme, svg):
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, f'{name}-{theme}.svg'), 'w', encoding='utf-8', newline='\n') as f:
        f.write(svg.render())


# ---------------------------------------------------------------- the figures

def main():
    # Write latency against offered load.
    imm = read('saturation_immediate.csv')
    eve = read('saturation_eventual.csv')
    pick = lambda rows, batch, col: [(num(r['offered_per_s']), num(r[col])) for r in rows if r['max_batch'] == batch]
    lines('write-latency-p99', 'Write latency (p99) against offered load',
          'One writer, group commit, 512 clients, a node and an edge per write. E1-disk.',
          [('immediate, max_batch 512', pick(imm, '512', 'p99_ms')),
           ('immediate, max_batch 128', pick(imm, '128', 'p99_ms')),
           ('eventual, max_batch 128', pick(eve, '128', 'p99_ms'))],
          'offered writes per second', 'p99 latency (ms, log)', yunit=' ms',
          note='Latency runs from each write’s scheduled time (open load). Data: docs/data/saturation_*.csv')
    lines('write-throughput', 'Writes committed against writes offered',
          'Past saturation the writer commits a fixed number per second and the rest waits. E1-disk.',
          [('immediate, max_batch 512', pick(imm, '512', 'done_per_s')),
           ('immediate, max_batch 128', pick(imm, '128', 'done_per_s')),
           ('eventual, max_batch 128', pick(eve, '128', 'done_per_s'))],
          'offered writes per second', 'committed writes per second (log)',
          note='Data: docs/data/saturation_immediate.csv, saturation_eventual.csv')

    # Import interference.
    rows = read('import_interference.csv')
    hbars('import-interference', 'An agent’s write latency (p99) during a 300 000-node import',
          'The agent writes 200 times a second for 12 s. E1-disk.',
          [(r['import'], [num(r['agent_p99_ms'])], 0 if r['import'].endswith('chunks of 2000') else (2 if 'alone' in r['import'] else False)) for r in rows],
          ' ms', log=True, note='Bulk lane with chunks of 2 000 is what the server does now. Data: docs/data/import_interference.csv')

    # Memory under a large graph, 512 MiB container.
    rows = [r for r in read('ram_graph.csv') if r['container'] == '512m' and 'crash' not in r['operation']]
    hbars('memory-512', 'Peak memory per operation in a 512 MiB container',
          '1 M nodes, 10 M edges (6.4 GB file), page cache 128 MiB, projection cache 64 MiB. E2.',
          [(r['operation'], [num(r['peak_mib'])], True) for r in rows],
          ' MiB', limit=(512, 'container limit'),
          note='Every operation completed. Data: docs/data/ram_graph.csv')
    rows = [r for r in read('ram_graph.csv') if r['container'] == '2g' and 'crash' not in r['operation'] and 'create' not in r['operation']]
    hbars('memory-2g', 'Peak memory per operation in a 2 GiB container',
          'Same graph, default caches (256 MiB of pages, 256 MiB of projections). E2.',
          [(r['operation'], [num(r['peak_mib'])], True) for r in rows],
          ' MiB', note='The 989 MiB query was refused by max_memory, as intended. Data: docs/data/ram_graph.csv')

    # check.
    rows = [r for r in read('check_cost.csv') if r['seconds']]
    hbars('check-cost', 'Integrity check of 10 M edges', 'Seconds; the first version, with every record in memory, was killed at 2 GiB. E2.',
          [(f"{r['version']}, {r['container']}", [num(r['seconds'])], 0 if 'now' in r['version'] else False) for r in rows],
          ' s', note='Data: docs/data/check_cost.csv')

    # Vector inserts.
    rows = read('vector_insert_progress.csv')
    columns('vector-insert', 'Inserting 1 M vectors of 128 dimensions',
            'Seconds per 100 000 vectors, transactions of 5 000, vector cache 128 MiB. E2 (2 GiB).',
            [f"{int(num(r['vectors']) / 1000)}k" for r in rows], [num(r['step_s']) for r in rows], ' s',
            'vectors in the index after the step', note='A larger vector cache halves the time (vector_insert_cache.csv). Data: docs/data/vector_insert_progress.csv')

    # HNSW.
    rows = read('hnsw_recall.csv')
    lines('hnsw-latency', 'Vector search latency against ef', '20 000 vectors of 128 dimensions in 64 clusters, top 10. E1-mem.',
          [('p99', [(num(r['ef']), num(r['p99_ms'])) for r in rows]),
           ('p50', [(num(r['ef']), num(r['p50_ms'])) for r in rows])],
          'ef (log)', 'latency (ms)', logy=False, yunit=' ms',
          note='Recall@10: 0.956 at ef 16, 0.996 at 32, 1.000 from 64. Data: docs/data/hnsw_recall.csv')
    rows = read('hnsw_churn.csv')
    dots('hnsw-churn', 'Vector search recall@10 as the index changes', 'ef 64, 20 000 vectors; before and after the repairs of 10.3. E1-mem.',
         [(r['state'], [num(r['recall_ef64_after']), num(r['recall_ef64_before_10_3'])]) for r in rows],
         ['after 10.3', 'before 10.3'], 0.90, 1.00)

    # Engine.
    rows = read('engine.csv')
    hbars('engine', 'Core operations, before and after the performance pass', 'Microseconds, log scale; in memory. E1-mem.',
          [(r['operation'], [num(r['before_us']), num(r['after_us'])], None) for r in rows],
          ' µs', log=True, series=['before (layout 2)', 'after (layout 3)'], note='Data: docs/data/engine.csv')

    # Algorithms.
    rows = [r for r in read('algorithms.csv') if num(r['ms'])]
    hbars('algorithms', 'Graph algorithms on 100 000 nodes and 500 000 edges', 'Milliseconds, log scale; in memory. E1-mem.',
          [(r['step'], [num(r['ms'])], True) for r in rows], ' ms', log=True, note='Data: docs/data/algorithms.csv')

    # Property index.
    rows = read('property_index.csv')
    hbars('property-index', 'GQ queries with and without a property index', 'Milliseconds, log scale; 100 000 :Person nodes. E1-mem.',
          [(r['query'], [num(r['without_index_ms']), num(r['with_index_ms'])], None) for r in rows],
          ' ms', log=True, series=['label scan', 'property index'], note='Data: docs/data/property_index.csv')

    # Concurrent writes.
    rows = read('concurrent_writes.csv')
    get = lambda mode, d: next(num(r['writes_per_s']) for r in rows if r['writes'] == mode and r['durability'] == d)
    hbars('concurrent-writes', 'Writes per second from 16 threads', 'Each write one node; 4 readers at the same time. E1-disk.',
          [(mode, [get(mode, 'immediate'), get(mode, 'eventual')], None) for mode in ('one transaction each', 'group commit')],
          '', series=['immediate durability', 'eventual durability'], note='Data: docs/data/concurrent_writes.csv')

    # Hostile queries, by outcome.
    rows = read('hostile_queries.csv')
    order = ['max_rows', 'max_memory', 'timeout', 'max_reads', 'value depth', 'nesting error', 'ok']
    rows.sort(key=lambda r: (order.index(r['outcome']), -num(r['ms'])))
    hbars('hostile-queries', 'Hostile GQ queries and what stopped each', 'Milliseconds until the answer, log scale; limits 2 s, 2 M reads, 200 000 rows, 64 MiB. E1-mem.',
          [(r['query'], [num(r['ms'])], order.index(r['outcome'])) for r in rows], ' ms', log=True,
          legend=[(o, i) for i, o in enumerate(order)],
          note='Every query ended within twice its 2 s timeout, without a crash. Data: docs/data/hostile_queries.csv')

    # Entity resolution.
    rows = read('resolve_quality.csv')
    hbars('resolve-quality', 'What a match rule decided, by kind of record', 'Share of records; 100 000 people, 2 000 damaged duplicates and 2 000 namesakes. E1-mem.',
          [(r['kind'], [num(r['same']) / num(r['records']) * 100, num(r['review']) / num(r['records']) * 100, num(r['missed']) / num(r['records']) * 100], None) for r in rows],
          '%', series=['same', 'review', 'different (missed for duplicates)'], top_value=100,
          note='For a namesake, “different” is right. Data: docs/data/resolve_quality.csv')
    rows = read('resolve_speed.csv')
    hbars('resolve-speed', 'Finding a person by a damaged name among 100 000', 'Milliseconds, p50, log scale. E1-mem.',
          [(r['search'], [num(r['p50_ms'])], 0 if 'fuzzy' in r['search'] else (2 if 'find_matches' in r['search'] else False)) for r in rows],
          ' ms', log=True, note='Data: docs/data/resolve_speed.csv')

    # Extraction runs.
    rows = read('extract_runs.csv')
    hbars('extract-runs', 'Extracting 20 000 memories, then again', 'Seconds, log scale; 5 000 known entities. Only new or changed texts are read. E1-mem.',
          [(f"{r['run']} ({r['texts_read']} read)", [num(r['seconds'])], 0 if r['run'] == 'first run' else 2) for r in rows],
          ' s', log=True, note='Data: docs/data/extract_runs.csv')

    print('charts written to', OUT)


if __name__ == '__main__':
    main()
