import { PLAN_ACTIONS } from './plan-actions.js';

const REPORT_KINDS = ['daily', 'weekly'];
const STATUS_PAGES = ['up', 'down'] as const;

export async function planRoutes(app: any) {
  for (const action of PLAN_ACTIONS) {
    app.post(`/teams/:id/plan/actions/${action}`, async (req: any, reply: any) => {
      return reply.send({ action, team: req.params.id });
    });
  }

  for (const kind of REPORT_KINDS) app.get(`/reports/${kind}`, async () => ({ kind }));

  STATUS_PAGES.forEach((page) => app.get(`/status/${page}`, async () => ({ page })));

  for (const v of ['v1', 'v2']) {
    for (const r of ['users', 'teams']) {
      app.get(`/${v}/${r}`, async () => []);
    }
  }

  for (const quiet of ['a', 'b']) app.get(`/quiet/${quiet}`, async () => null);
}
