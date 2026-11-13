import type { App } from 'fixture-plugin-server';
import { statusRoutes } from './errors';

export async function main(server: App) {
  server.register(statusRoutes, { prefix: '/internal' });
}
