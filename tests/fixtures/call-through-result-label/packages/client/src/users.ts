import { api } from "fixture-reply-client";

export interface User {
  id: string;
}

export interface UserPage {
  items: User[];
}

function toModel(user: User) {
  return { id: user.id, loadedAt: Date.now() };
}

/** A member read: the store labels `data` of a reply as the body. */
export async function loadUser(id: string) {
  const reply = await api.get<User>(`/users/${id}`);
  return reply.data as User;
}

/** A member read that is not the labelled one. */
export async function loadStatus(id: string) {
  return (await api.get<User>(`/users/${id}/status`)).status;
}

/** The caller supplies the whole return, and the store labels it whole. */
export function createUser(name: string) {
  return api.send<User>("/users", { name });
}

/** A member read over a call the store labels with nothing. */
export async function pingUsers() {
  return (await api.ping("/ping")).ok;
}

/** The body's own member: two reads, the second of the body. */
export async function loadPage(id: string) {
  const reply = await api.get<UserPage>(`/users/${id}/page`);
  return reply.data.items;
}

/** The body is built into another value before it is returned. */
export async function loadModel(id: string) {
  return toModel((await api.get<User>(`/users/${id}/model`)).data);
}

/** The body goes through a function the scan cannot read. */
export async function loadFrozen(id: string) {
  return Object.freeze(await api.send<User>(`/users/${id}/frozen`));
}

/** The body goes through `.then`. */
export function loadThen(id: string) {
  return api.get<User>(`/users/${id}/then`).then((reply) => reply.data);
}

/** The body goes through `.catch`. */
export function loadCaught(id: string) {
  return api.send<User>(`/users/${id}/caught`).catch(() => null);
}
