import { itemRoutes } from './items';
import { orderRoutes } from './orders';

export async function api(app: any) {
  app.register(
    async (api: any) => {
      await api.register(itemRoutes);
    },
    { prefix: '/a' },
  );
  app.register(
    async (api: any) => {
      await api.register(orderRoutes);
    },
    { prefix: '/b' },
  );
}
