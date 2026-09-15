import { builder } from '../builder';
import { findWidget, listWidgets } from './store';

builder.queryFields((t) => ({
  widgets: t.field({
    type: ['Widget'],
    resolve: () => listWidgets(),
  }),
  widget: t.field({
    type: 'Widget',
    nullable: true,
    args: { id: t.arg.id({ required: true }) },
    resolve: (_root, args) => findWidget(String(args.id)),
  }),
}));
