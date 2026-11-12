import type { App } from 'fixture-plugin-server';
import { reportRoutes } from './reports';

export async function api(app: App, opts: { prefix?: string }) {
  app.register(
    async (api: App) => {
      await api.register(reportRoutes);
    },
    { prefix: opts.prefix ?? '/api/v1' },
  );
}
