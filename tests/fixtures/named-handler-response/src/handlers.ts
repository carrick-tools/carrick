import type { Next, Req, Res } from '@fixture-http/router';
import { listAll, store, type NewWidget, type Widget } from './widgets';

// Returns its payload: the declared return is the body.
export async function listWidgets(_req: Req, _res: Res): Promise<Widget[]> {
  return listAll();
}

// Returns the send it makes.
export const showWidget = async (req: Req, res: Res) => {
  const widgets = await listAll();
  return res.json({ widget: widgets[0], asked: req.params.id });
};

// Sends through the response it was handed and returns nothing.
export const createWidget = async (req: Req<NewWidget>, res: Res) => {
  const created = await store(req.body);
  res.status(201).json(created);
};

// Ends the response without a body.
export const removeWidget = async (_req: Req, res: Res) => {
  res.status(204).end();
};

// Sends on the path that succeeds and hands a failure on.
export const auditWidget = async (req: Req, res: Res, next: Next) => {
  try {
    const widgets = await listAll();
    res.json({ audited: widgets.length, by: req.params.id });
  } catch (error) {
    return next(error);
  }
};

// Answers with an error status and nothing else.
export const retiredWidget = async (_req: Req, res: Res) => {
  res.status(410).json({ error: 'gone' });
};

// A gate in front of a handler: it refuses with a body of its own, or passes
// the request on.
export const requireKey = (req: Req, res: Res, next: Next) => {
  if (!req.params.key) {
    res.status(401).json({ error: 'no key' });
    return;
  }
  next();
};

// Imported under another name by the module that registers it.
export const archiveWidget = async (req: Req, res: Res) => {
  res.json({ archived: req.params.id });
};
