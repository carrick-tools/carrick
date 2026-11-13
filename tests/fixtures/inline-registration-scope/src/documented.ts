import type { App } from 'fixture-plugin-server';
import { itemDetailRoutes } from './item-detail';

export async function api(app: App, opts: { prefix?: string }) {
  app.register(
    async (api: App) => {
      await api.register(itemDetailRoutes);
    },
    { prefix: opts.prefix ?? '/api/v1' },
  );
}
