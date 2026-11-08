import { ListUsersDocument } from './generated/documents';
import { useQuery } from './client';

export function useUsers(): unknown {
  return useQuery({ query: ListUsersDocument });
}
