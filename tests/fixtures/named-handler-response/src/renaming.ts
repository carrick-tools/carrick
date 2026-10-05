import { guarded } from './guarded';
import { store, type NewWidget } from './widgets';

// Bound to a call that wraps the function, not to the function.
export const renameWidget = guarded<NewWidget>(async (req, res) => {
  const renamed = await store(req.body);
  res.json(renamed);
});
