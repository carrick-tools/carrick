/**
 * Schema-first route registrations (carrick#528).
 *
 * Every handler here is a thin arrow that forwards to a controller function, so
 * the route file contains NO request read and NO payload expression — the only
 * places the contract is written down are the handler's parameter annotation
 * and the registration's `schema` object.
 *
 *  - `POST /widgets` declares both: a typed request parameter AND a schema.
 *  - `POST /widgets/import` declares only the schema (its request parameter is
 *    the unparameterised request type, whose `body` is `unknown`).
 *  - `POST /widgets/legacy` declares neither, and casts inside the handler —
 *    the pre-existing typed-request-read anchor must still win there.
 */

import type { Reply, RouteRequest, Server } from './route-schema-lib';
import { $ref, type CreateWidgetRequest } from './route-schema-registry';

export interface LegacyWidget {
  legacyId: number;
  label: string;
}

export function registerWidgetRoutes(server: Server): void {
  server.post(
    '/widgets',
    {
      schema: {
        body: $ref('CreateWidget'),
        response: {
          200: $ref('WidgetView'),
        },
      },
    },
    async (request: CreateWidgetRequest, reply: Reply) =>
      createWidget(request, reply)
  );

  server.post(
    '/widgets/import',
    {
      schema: {
        body: $ref('ImportWidgets'),
      },
    },
    async (request: RouteRequest, reply: Reply) => importWidgets(request, reply)
  );

  server.post(
    '/widgets/legacy',
    {},
    async (request: RouteRequest, reply: Reply) => {
      const widget = request.body as LegacyWidget;
      return reply.send(widget.label);
    }
  );
}

async function createWidget(
  request: CreateWidgetRequest,
  reply: Reply
): Promise<unknown> {
  return reply.send({ id: 'w-1', ...request.body });
}

async function importWidgets(
  request: RouteRequest,
  reply: Reply
): Promise<unknown> {
  return reply.send({ imported: 0, source: request.body });
}
