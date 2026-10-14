declare function listActions(): string[];
declare function load(): string[];
declare const h: any;

const BASE = ['a'];
const MIXED = [...BASE, 'z'];
let LATER = ['a'];
const GROWN = ['a'];
GROWN.push('b');
const PLAN_ACTIONS = ['x'];

export async function unreadRoutes(app: any, options: { paths: string[] }) {
  for (const action of listActions()) app.post(`/x/${action}`, h);
  for (const p of options.paths) app.get(`/meta${p}`, h);
  for (const m of MIXED) app.get(`/mixed/${m}`, h);
  for (const l of LATER) app.get(`/later/${l}`, h);
  for (const g of GROWN) app.get(`/grown/${g}`, h);
  {
    const PLAN_ACTIONS = load();
    for (const s of PLAN_ACTIONS) app.get(`/shadow/${s}`, h);
  }
  const V = 'v1';
  for (const r of ['a']) app.get(`/${V}/${r}`, h);
  return PLAN_ACTIONS;
}
