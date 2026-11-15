import { http } from "@fixture/http";

function request<T>(path: string) {
  return http.get<T>(path);
}

export interface Thing {
  id: string;
  name: string;
}

export function getThing(id: string) {
  return request<Thing>(`/things/${id}`);
}

export function listThings(kind: string) {
  return request<Thing[]>(`/things/kinds/${kind}`);
}

export function saveThing(thing: Thing) {
  return http.post<Thing>(`/things/${thing.id}`, thing);
}
