import { itemsQuery } from '@fixture/documents';
import { gql } from './client';

function watch<T>(f: () => T): () => T {
  return f;
}

export class Store {
  refresh = watch(() => gql({ query: itemsQuery }));
}
