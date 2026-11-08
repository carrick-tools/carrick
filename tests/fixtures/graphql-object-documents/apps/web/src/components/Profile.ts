import { getCurrentUserQuery, listUsersQuery, updateUserMutation } from '../generated';

declare function refresh(document: unknown): void;

export function load(): void {
  refresh(getCurrentUserQuery);
}

export class Admin {
  list(): void {
    refresh(listUsersQuery);
  }
  save(): void {
    refresh(updateUserMutation);
  }
}
