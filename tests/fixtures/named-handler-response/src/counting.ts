import type { Req, Res } from '@fixture-http/router';
import { listAll } from './widgets';

export const countWidgets = async (_req: Req, res: Res) => {
  const widgets = await listAll();
  res.json({ count: widgets.length });
};
