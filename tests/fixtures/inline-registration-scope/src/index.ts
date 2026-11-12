import type { App } from 'fixture-plugin-server';
import { api } from './nullish';

export async function main(server: App) {
  server.register(api, { prefix: '/root' });
}
