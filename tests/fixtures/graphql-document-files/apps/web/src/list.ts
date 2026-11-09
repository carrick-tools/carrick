import { itemsQuery } from '@fixture/documents';
import { gql } from './client';

export function load(): Promise<unknown> {
  return gql({ query: itemsQuery });
}

export function maybe(filter?: string): Promise<unknown> {
  return gql(filter && { query: itemsQuery, variables: { filter } });
}
