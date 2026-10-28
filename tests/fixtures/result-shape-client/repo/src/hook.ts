import { useLoad } from 'fixture-reply-client';
import { User } from './client';

// 5. A hook: the value its function resolves to is the result's `data`.
export function useUser(id: string) {
  return useLoad({
    key: id,
    load: async (): Promise<User> => ({ id }),
  });
}
