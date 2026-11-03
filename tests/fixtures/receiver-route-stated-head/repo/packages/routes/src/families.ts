import type { App } from 'fixture-plugin-server';

const KINDS = ['small', 'large'] as const;

export interface FamilyOptions {
  specPaths: readonly string[];
  heads: readonly string[];
}

// Route paths that write their first segment and then a value the code does
// not read, and one that starts with the value.
export async function familyRoutes(app: App, opts: FamilyOptions) {
  for (const kind of KINDS) {
    app.post(`/widgets/kinds/${kind}`, async () => ({ added: true }));
  }
  for (const tail of opts.specPaths) {
    app.get(`/docs/spec${tail}`, async () => ({ spec: true }));
  }
  app.get('/docs/spec', async () => ({ spec: true }));
  for (const head of opts.heads) {
    app.get(`${head}health`, async () => ({ ok: true }));
  }
}
