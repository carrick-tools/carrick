import type { App } from 'fixture-plugin-server';
import { itemRoutes } from './items';
import { orderRoutes } from './orders';

export async function api(app: App) {
  app.register(
    async (api: App) => {
      await api.register(itemRoutes);
    },
    { prefix: '/a' },
  );
  app.register(
    async (api: App) => {
      await api.register(orderRoutes);
    },
    { prefix: '/b' },
  );
}
