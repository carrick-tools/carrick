import { findWidget, listWidgets, saveWidget } from '../store';
import { wrap } from '../utils/wrap';

// Each member is a callback handed to a wrapper call, and the object is
// exported where it is declared.
export const widgetController = {
  list: wrap(async (req, res) => {
    res.json(await listWidgets());
  }),
  get: wrap(async (req, res) => {
    res.json(await findWidget(req.params.id));
  }),
  create: wrap(async (req, res) => {
    res.status(201).json(await saveWidget(req.body));
  }),
};
