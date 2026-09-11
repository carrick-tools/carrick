/**
 * Routes whose request contract is declared by a VALIDATOR MIDDLEWARE on the
 * registration (carrick#964).
 *
 * The handler takes one context object. That context's `body` member is the
 * response sender, so reading `body` off the handler's parameter type answers a
 * callable — the framework's own machinery — rather than the request payload.
 * The payload is declared where the middleware binds the schema to the request
 * part, and read back through the context's validated input.
 *
 *  - `POST /search` declares its body with a validator middleware.
 *  - `POST /reports` validates a NON-body part, so the route declares no
 *    request body at all.
 *  - `GET /health` declares nothing and sends an empty body through the
 *    context's `body` member.
 */

import { Router, validator, type HttpResponse } from 'context-framework';
import * as schema from 'schema-lib';

const SearchPayload = schema.object({
  term: schema.string(),
  regions: schema.optional(schema.array(schema.string())),
});

const ReportQuery = schema.object({
  since: schema.string(),
});

export const router = new Router();

router.post('/search', validator('json', SearchPayload), async (c) => {
  const payload = c.req.valid('json');
  return c.json({ term: payload.term, regions: payload.regions ?? [] });
});

router.post('/reports', validator('query', ReportQuery), async (c) => {
  const query = c.req.valid('query');
  return c.json({ since: query.since });
});

router.get('/health', async (c): Promise<HttpResponse> => {
  return c.body(null, 204);
});
