// Bookkeeping about routes: plain data, not routes.
const RECORDS = [
  { method: 'GET', path: '/shipments/:id', line: 12, source: 'fixture' },
  { method: 'POST', path: '/shipments', line: 20, source: 'fixture' },
];

const HASH_INPUT = {
  routes: [{ service: 'shipments', repo: 'acme/shipments', method: 'POST', path: '/shipments' }],
};

export function describeRecords() {
  return RECORDS.map((record) => `${record.method} ${record.path}`);
}

export function hashInput() {
  return JSON.stringify(HASH_INPUT);
}
