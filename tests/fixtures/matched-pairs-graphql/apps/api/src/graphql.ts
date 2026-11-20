import type { Owner } from './owner';

const owner: Owner = { name: 'o1' };

export const resolvers = {
  Query: {
    widget: {
      resolve: (): { id: number; total: number; owner: Owner } => ({
        id: 1,
        total: 3,
        owner,
      }),
    },
  },
  Mutation: {
    renameWidget: {
      resolve: (): { id: number; total: number } => ({ id: 1, total: 3 }),
    },
  },
};
