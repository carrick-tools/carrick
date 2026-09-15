/**
 * Routes whose declared request schema has an INPUT that differs from its
 * OUTPUT (carrick#1101).
 *
 * A caller sends the schema's input; the handler receives its output. A key
 * with a default is optional to send and present after parsing, a transform
 * changes the member's type, and a coercion accepts any value on input.
 *
 *  - `POST /profile`   validator middleware, defaulted keys.
 *  - `POST /tags`      validator middleware, a transformed member.
 *  - `POST /page`      validator middleware, a coerced member.
 *  - `POST /standard`  validator middleware, a schema exposing only Standard Schema.
 *  - `POST /parse-only` validator middleware, a schema exposing only `parse`.
 *  - `/profile/schema` and `/standard/schema` declare the same schemas in a
 *    route `schema` object, as both the body and the 200 response.
 */

import { Router, validator } from 'context-framework';
import * as schema from 'schema-lib';
import * as standard from 'standard-lib';
import type { Reply, RouteRequest, Server } from './route-schema-lib';

const ProfilePayload = schema.object({
  displayName: schema.string(),
  theme: schema.defaulted(schema.string(), 'light'),
  notify: schema.defaulted(schema.boolean(), true),
});

const TagsPayload = schema.object({
  tags: schema.transform(schema.string(), (value) => value.split(',')),
});

const PagePayload = schema.object({
  page: schema.defaulted(schema.coerce.number(), 1),
  label: schema.string(),
});

const StandardPayload = standard.object({
  title: standard.string(),
  priority: standard.withDefault(standard.number(), 3),
});

const ParseOnlyPayload = schema.parseOnly<{ ticket: string }>();

export const router = new Router();

router.post('/profile', validator('json', ProfilePayload), async (c) => {
  const profile = c.req.valid('json');
  return c.json({ theme: profile.theme });
});

router.post('/tags', validator('json', TagsPayload), async (c) => {
  const tagged = c.req.valid('json');
  return c.json({ count: tagged.tags.length });
});

router.post('/page', validator('form', PagePayload), async (c) => {
  const paged = c.req.valid('form');
  return c.json({ page: paged.page });
});

router.post('/standard', validator('json', StandardPayload), async (c) => {
  const task = c.req.valid('json');
  return c.json({ priority: task.priority });
});

router.post('/parse-only', validator('json', ParseOnlyPayload), async (c) => {
  const ticketed = c.req.valid('json');
  return c.json({ ticket: ticketed.ticket });
});

export function registerSchemaRoutes(server: Server): void {
  server.post(
    '/profile/schema',
    {
      schema: {
        body: ProfilePayload,
        response: {
          200: ProfilePayload,
        },
      },
    },
    async (request: RouteRequest, reply: Reply) => reply.send(request.body)
  );

  server.post(
    '/standard/schema',
    {
      schema: {
        body: StandardPayload,
        response: {
          200: StandardPayload,
        },
      },
    },
    async (request: RouteRequest, reply: Reply) => reply.send(request.body)
  );
}
