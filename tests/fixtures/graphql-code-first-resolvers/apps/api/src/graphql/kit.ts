import SchemaKit from '@example/schema-kit';

export interface Parcel {
  id: string;
  weightGrams: number;
}

export const kit = new SchemaKit<{ Objects: { Parcel: Parcel } }>({});

kit.objectType('Parcel', {
  fields: (t) => ({
    id: t.exposeID('id'),
    weightGrams: t.exposeInt('weightGrams'),
  }),
});

kit.queryType({
  fields: (t) => ({
    health: t.string({ resolve: () => 'ok' }),
  }),
});

kit.mutationType({});
