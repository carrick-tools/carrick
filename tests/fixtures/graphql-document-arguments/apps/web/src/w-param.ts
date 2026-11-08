import { currentUserQuery } from './generated/documents';
import { gql } from './client';

function run(doc: unknown): Promise<unknown> {
  return gql({ query: doc });
}

export function loadCurrentUser(): Promise<unknown> {
  return run(currentUserQuery);
}
