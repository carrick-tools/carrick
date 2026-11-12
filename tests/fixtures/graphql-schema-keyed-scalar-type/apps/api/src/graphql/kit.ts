import SchemaKit from '@example/schema-kit';

export const kit = new SchemaKit<{
  Scalars: { DateTime: { Input: Date; Output: Date } };
}>({});

// The wire value of a DateTime is its ISO string.
kit.scalarType('DateTime', {
  serialize: (value: Date) => value.toISOString(),
  parseValue: (value: unknown) => new Date(String(value)),
});

kit.queryType({
  fields: (t) => ({
    health: t.string({ resolve: () => 'ok' }),
  }),
});

kit.mutationType({});
