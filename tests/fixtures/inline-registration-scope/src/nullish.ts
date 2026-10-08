import { itemRoutes } from './items';

export async function api(app: any, opts: { prefix?: string }) {
  app.register(
    async (api: any) => {
      await api.register(itemRoutes);
      api.get('/health', async () => ({ ok: true }));
    },
    { prefix: opts.prefix ?? '/api/v1' },
  );
}
