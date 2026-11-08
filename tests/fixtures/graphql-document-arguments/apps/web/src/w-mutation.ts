import { updateUserMutation } from './generated/documents';
import { useMutation } from './client';

export function useRename(): unknown {
  return useMutation({ mutation: updateUserMutation });
}
