import { itemsQuery } from '@fixture/documents';
import { gql } from './client';

export function load(): Promise<unknown> {
  return gql({ query: itemsQuery });
}
