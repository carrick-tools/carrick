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
 *  - `POST /mixed`     validator middleware, a coerced member beside a
 *    defaulted one (carrick#1105): each member keeps its own direction.
 *  - `POST /nested`    validator middleware, coerced and defaulted members
 *    inside a nested object, an array, and an array of objects, plus an
 *    optional coerced key (its output is a union with `undefined`).
 *  - `POST /pair`      validator middleware, a coerced tuple element the
 *    printer keeps by name, beside a defaulted key.
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

const MixedPayload = schema.object({
  page: schema.coerce.number(),
  theme: schema.defaulted(schema.string(), 'light'),
  label: schema.string(),
});

const NestedPayload = schema.object({
  filter: schema.object({
    limit: schema.coerce.number(),
    sort: schema.defaulted(schema.string(), 'asc'),
  }),
  ids: schema.array(schema.coerce.number()),
  lines: schema.array(
    schema.object({
      qty: schema.coerce.number(),
      note: schema.defaulted(schema.string(), ''),
    })
  ),
  label: schema.defaulted(schema.string(), 'none'),
  offset: schema.optional(schema.coerce.number()),
});

const PairPayload = schema.object({
  point: schema.pair(schema.coerce.number(), schema.string()),
  unit: schema.defaulted(schema.string(), 'px'),
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

router.post('/mixed', validator('json', MixedPayload), async (c) => {
  const mixed = c.req.valid('json');
  return c.json({ page: mixed.page, theme: mixed.theme });
});

router.post('/nested', validator('json', NestedPayload), async (c) => {
  const nested = c.req.valid('json');
  return c.json({ limit: nested.filter.limit });
});

router.post('/pair', validator('json', PairPayload), async (c) => {
  const placed = c.req.valid('json');
  return c.json({ x: placed.point[0] });
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
