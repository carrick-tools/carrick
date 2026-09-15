import { builder } from '../builder';
import { createWidget, disposeWidget } from './store';

builder.mutationFields((t) => ({
  createWidget: t.field({
    type: 'Widget',
    args: { name: t.arg.string({ required: true }) },
    resolve: (_root, args) => createWidget(args.name),
  }),
  disposeWidget: t.boolean({
    args: { id: t.arg.id({ required: true }) },
    resolve: (_root, args) => disposeWidget(String(args.id)),
  }),
}));
