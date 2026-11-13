import type { App } from 'fixture-plugin-server';
import { onError } from './errors';

export async function api(app: App, opts: { prefix?: string }) {
  app.register(
    async (api: App) => {
      api.setErrorHandler(onError);
    },
    { prefix: opts.prefix ?? '/api/v1' },
  );
}
