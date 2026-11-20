import { findGadget, listGadgets, removeGadget } from '../store';
import { wrap } from '../utils/wrap';

// The same table, declared first and exported by a later statement, with one
// member written as a function expression.
const gadgetController = {
  list: wrap(async (req, res) => {
    res.json(await listGadgets());
  }),
  get: wrap(function (req, res) {
    res.json(findGadget(req.params.id));
  }),
  remove: wrap(async (req, res) => {
    await removeGadget(req.params.id);
    res.status(204).end();
  }),
};

export default gadgetController;
