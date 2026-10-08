import { itemRoutes } from './items';

export async function api(app: any, opts: { legacy?: boolean }) {
  app.register(
    async (api: any) => {
      await api.register(itemRoutes);
    },
    { prefix: opts.legacy ? '/legacy' : '/api/v3' },
  );
}
