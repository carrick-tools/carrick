import type { App } from 'fixture-aliased-server';
import { ping } from './handlers';

export interface CatalogOptions {
  version?: string;
}

// The plugin's routes sit on its own app, and a nested plugin's under the
// version the caller states, or `/v1` where it states none.
export async function catalogPlugin(instance: App, opts: CatalogOptions) {
  instance.get('/products', ping);
  await instance.register(
    async (inner: App) => {
      inner.post('/orders', ping);
    },
    { prefix: opts.version ?? '/v1' },
  );
}
