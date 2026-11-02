import type { App } from 'fixture-plugin-server';
import { widgetRoutes } from './widgets';

export interface RoutesOptions {
  routePrefix?: string;
}

// Every route of this package is registered on an app it is handed, under
// one scope whose prefix defaults here. The package never builds an app, so
// it uses the server package only through its types.
export async function apiRoutes(app: App, opts: RoutesOptions) {
  await app.register(
    async (api: App) => {
      await api.register(widgetRoutes);
    },
    { prefix: opts.routePrefix ?? '/api/v1' },
  );
}
