import type { App, Handler } from 'fixture-route-server';

export interface RouteDef {
  method: 'GET' | 'POST';
  path: string;
  handler: Handler;
}

export function addRoute(app: App, def: RouteDef): void {
  if (def.method === 'POST') {
    app.post(def.path, def.handler);
  } else {
    app.get(def.path, def.handler);
  }
}
