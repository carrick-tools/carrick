import { itemRoutes } from './items';

const API_PREFIX = '/api/v5';

export async function api(app: any) {
  app.register(
    async (api: any) => {
      await api.register(itemRoutes);
    },
    { prefix: API_PREFIX },
  );
}
