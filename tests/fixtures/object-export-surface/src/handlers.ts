// The other export-surface-as-object shape: a named table of handlers, with a
// method, an arrow-valued property, and one level of nesting.
export const handlers = {
  async list(): Promise<string[]> {
    return [];
  },
  create: async (name: string): Promise<string> => {
    return name;
  },
  archive: {
    restore(id: string): string {
      return id;
    },
  },
};

// The bound: an object the module keeps to itself is a value it uses, not a
// surface it offers, and its members are not indexed.
const internals = {
  tidy(input: string): string {
    return input.trim();
  },
};

export function normalize(input: string): string {
  return internals.tidy(input);
}
