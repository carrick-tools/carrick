declare const h: any;

const REPORT_KINDS = ['daily', 'weekly'];

export const keptDescriptors = [{ method: 'GET', path: '/health', handler: h }];

export async function keptRoutes(app: any) {
  app.get('/:id', h);
  app.get('/*', h);
  for (const kind of REPORT_KINDS) {
    await fetch(`/reports/${kind}`);
  }
}
