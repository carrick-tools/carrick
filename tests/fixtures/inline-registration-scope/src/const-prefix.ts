import type { App } from 'fixture-plugin-server';
import { itemRoutes } from './items';

const API_PREFIX = '/api/v5';

export async function api(app: App) {
  app.register(
    async (api: App) => {
      await api.register(itemRoutes);
    },
    { prefix: API_PREFIX },
  );
}
