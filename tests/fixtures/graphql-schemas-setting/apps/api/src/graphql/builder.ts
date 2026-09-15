import SchemaBuilder from '@pothos/core';

export interface Widget {
  id: string;
  name: string;
}

export const builder = new SchemaBuilder<{ Objects: { Widget: Widget } }>({});

builder.objectType('Widget', {
  fields: (t) => ({
    id: t.exposeID('id'),
    name: t.exposeString('name'),
  }),
});

builder.queryType({
  fields: (t) => ({
    health: t.string({ resolve: () => 'ok' }),
  }),
});

builder.mutationType({});
