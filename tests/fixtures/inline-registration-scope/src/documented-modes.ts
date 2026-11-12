import { reportRoutes } from './reports';

export async function api(app: any, opts: { prefix?: string }) {
  app.register(
    async (api: any) => {
      await api.register(reportRoutes);
    },
    { prefix: opts.prefix ?? '/api/v1' },
  );
}
