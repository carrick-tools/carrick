import server from 'fixture-plugin-server';
import { OPEN, PLAN_ACTIONS } from './kinds';
import { listActions, ok } from './actions';

const app = server();
const h = async () => ({ ok: true });
const cond = process.env.ROUTE_FLAG === '1';

const SIZES = ['small', 'large'] as const;
const BASE = ['p', 'q'];
const MIXED = [...BASE, 'z'];
let LATER = ['a'];
const GROWN = ['a'];
GROWN.push('b');
const PAGES = ['up', 'down'];

// Stated: each element is a route.
export function sizeRoutes() {
  for (const size of SIZES) {
    app.post(`/widgets/sizes/${size}`, async () => ({ size }));
  }
}

for (const action of PLAN_ACTIONS) {
  app.post(`/teams/:id/plan/${action}`, async () => ({ action }));
}

for (const [file, type] of [['favicon.svg', 'image/svg+xml'], ['logo.png', 'image/png']] as const) {
  if (ok(file)) app.get(`/${file}`, async () => ({ type }));
}

for (const [a, b] of [['x', '1'], ['y', '2']] as const) app.get(`/pair/${a}/${b}`, h);

for (const v of ['v1', 'v2']) for (const r of ['users', 'teams']) app.get(`/${v}/${r}`, h);

// Unread: the code does not state the list, or the list may change.
for (const x of listActions()) app.post(`/x/${x}`, h);

export function metaRoutes(opts: { paths: string[] }) {
  for (const p of opts.paths) app.get(`/meta/${p}`, h);
}

for (const m of MIXED) app.get(`/mixed/${m}`, h);

for (const l of LATER) app.get(`/later/${l}`, h);

for (const g of GROWN) app.get(`/grown/${g}`, h);

for (const o of OPEN) app.get(`/open/${o}`, h);

for (const c1 of ['a', 'b', 'c', 'd', 'e']) for (const c2 of ['f', 'g', 'h', 'i']) app.get(`/cap/${c1}/${c2}`, h);

PAGES.forEach((page) => app.get(`/status/${page}`, h));

// Kept as they are.
app.get('/:id', h);

app.get(cond ? '/p' : '/q', h);

for (const k of SIZES) fetch(`/sizes/${k}`);

// A guard in the loop body: an element the test of a literal skips states
// no route; a test of anything else keeps every element.
for (const k of ['a', 'b', 'x']) {
  if (k === 'x') continue;
  app.get(`/skip/${k}`, h);
}

for (const k of ['a', 'b', 'x']) {
  if (k !== 'x') {
    app.get(`/only/${k}`, h);
  }
}

for (const k of ['a', 'b', 'x']) {
  if (ok(k)) continue;
  app.get(`/unsure/${k}`, h);
}

for (const [file, kind] of [['a.svg', 'svg'], ['b.png', 'png']]) {
  if ('png' === kind) continue;
  app.get(`/asset/${file}`, h);
}
