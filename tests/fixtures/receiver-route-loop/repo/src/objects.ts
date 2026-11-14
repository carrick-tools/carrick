import server from 'fixture-plugin-server';

const app = server();
const h = async () => ({ ok: true });
declare function make(): string;

const ROUTES = [
  { path: '/reports/daily', verb: 'get' as const },
  { path: '/reports/weekly', verb: 'post' as const },
];

// Stated: each element's path is a route.
for (const r of ROUTES) app.get(r.path, h);

for (const r of [{ base: '/v1', leaf: 'jobs' }, { base: '/v2', leaf: 'tasks' }]) app.get(`${r.base}/${r.leaf}`, h);

// Unread: one element's path is a call's value.
for (const r of [{ path: '/queue/a' }, { path: make() }]) app.post(r.path, h);

// The method read off the element names the member: a computed callee.
for (const r of [{ path: '/moves/a', verb: 'get' as const }, { path: '/moves/b', verb: 'post' as const }]) app[r.verb](r.path, h);
