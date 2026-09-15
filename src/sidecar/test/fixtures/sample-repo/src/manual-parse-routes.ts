/**
 * Request rows the route declares nowhere but its handler body (carrick#1166).
 *
 * Written without semicolons on purpose: a registration statement then spans
 * exactly the bytes of its call, which is the shape a whole-registration span
 * locator has to resolve to the CALL, not the statement around it.
 *
 *  - `POST /notes` reads an untyped body and hands it to `safeParse`.
 *  - `POST /notes/strict` hands the read straight to `parse`.
 *  - `POST /notes/:id/archive` validates a path parameter, which is not a body.
 *  - `POST /jobs/run` reads no body at all.
 *  - `POST /notes/loose` reads a body and never validates it.
 */

import { Router, validator } from 'context-framework'
import * as schema from 'schema-lib'

const CreateNote = schema.object({
  title: schema.string(),
  tags: schema.optional(schema.array(schema.string())),
})

const NoteParams = schema.object({
  id: schema.string(),
})

export const noteRouter = new Router()

noteRouter.post('/notes', async (c) => {
  const body = await c.req.json()
  const parsed = CreateNote.safeParse(body)
  if (!parsed.success) {
    return c.json({ error: parsed.error.message }, 400)
  }
  return c.json({ title: parsed.data.title })
})

noteRouter.post('/notes/strict', async (c) => {
  const input = CreateNote.parse(await c.req.json())
  return c.json({ title: input.title })
})

noteRouter.post('/notes/:id/archive', validator('param', NoteParams), async (c) => {
  const params = c.req.valid('param')
  return c.json({ archived: params.id })
})

noteRouter.post('/jobs/run', async (c) => {
  return c.json({ started: true })
})

noteRouter.post('/notes/loose', async (c) => {
  const payload = await c.req.json()
  return c.json({ received: payload !== null })
})

declare function sendNote(init: { method: string; body: string }): Promise<unknown>

/** A consumer whose body is a parameter of the arrow it is declared in. */
export const createNote = (input: { title: string; tags: string[] }) =>
  sendNote({
    method: 'POST',
    body: JSON.stringify(input),
  })
