import { userByIdQuery } from '@fixture/documents';
import { gql } from './client';

export function loadUser(id: string): Promise<unknown> {
  return gql({ query: userByIdQuery, variables: { id } });
}
