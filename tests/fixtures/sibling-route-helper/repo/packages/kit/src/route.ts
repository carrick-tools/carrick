import { route } from 'fixture-route-server';
import type { Handler } from 'fixture-route-server';

export interface RouteDef {
  path: string;
  handler: Handler;
}

export function addRoute(def: RouteDef): void {
  route(def.path, def.handler);
}
