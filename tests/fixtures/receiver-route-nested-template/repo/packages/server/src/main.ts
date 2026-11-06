import server from 'fixture-plugin-server';
import { LOGIN_PATH, PROFILE_PATH, STATUS_PATH } from '@fx/paths';
import { RESET_PATH } from './paths';

const app = server();

export function start() {
  app.get(LOGIN_PATH, async () => ({ ok: true }));
  app.get(`${LOGIN_PATH}/:id`, async () => ({ ok: true }));
  app.post(`${PROFILE_PATH}/:section`, async () => ({ saved: true }));
  app.get(`${STATUS_PATH}/:id`, async () => ({ ok: true }));
  app.get(RESET_PATH, async () => ({ ok: true }));
  app.get(`${RESET_PATH}/:token`, async () => ({ ok: true }));
  return app;
}
