import { useLoad } from "fixture-reply-client";
import {
  createUser,
  loadCaught,
  loadFrozen,
  loadModel,
  loadPage,
  loadStatus,
  loadThen,
  loadUser,
  pingUsers,
} from "@fixture/result-client";
import type { Status, User } from "./types";

/** Kept: the wrapper reads the member the store labels as the body. */
export async function showUser(id: string) {
  const user: User = await loadUser(id);
  return user;
}

/** Kept: the wrapper hands the whole value back, and the store labels it whole. */
export async function makeUser(name: string) {
  const user: User = await createUser(name);
  return user;
}

/** Dropped: the wrapper reads a member the store does not label. */
export async function showStatus(id: string) {
  const status: Status = await loadStatus(id);
  return status;
}

/** Dropped: the call the wrapper reads is labelled with nothing. */
export async function showPing() {
  const alive: Status = await pingUsers();
  return alive;
}

/** Dropped: the wrapper reads a member of the body as well. */
export async function showPage(id: string) {
  const users: User = await loadPage(id);
  return users;
}

/** Dropped: the body is built into another value. */
export async function showModel(id: string) {
  const user: User = await loadModel(id);
  return user;
}

/** Dropped: the body goes through a function the scan cannot read. */
export async function showFrozen(id: string) {
  const user: User = await loadFrozen(id);
  return user;
}

/** Dropped: the body goes through `.then`. */
export async function showThen(id: string) {
  const user: User = await loadThen(id);
  return user;
}

/** Dropped: the body goes through `.catch`. */
export async function showCaught(id: string) {
  const user: User | null = await loadCaught(id);
  return user;
}

/** Dropped: a hook's callback decides the row, not the wrapper's own value. */
export function userQuery(id: string) {
  const query = useLoad({
    key: id,
    load: async (): Promise<User> => {
      const user = await loadUser(id);
      return user;
    },
  });
  return query;
}
