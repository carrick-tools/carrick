import { http } from "@fixture/http";
import { listThings } from "./things";

export function directFlag(archived: boolean) {
  return http.get(`/direct/${archived ? "archived" : "active"}`);
}

const MODE = process.env.PREVIEW ? "preview" : "live";

export function directMode() {
  return http.get(`/direct/${MODE}`);
}

export function directEnv() {
  return http.get(`/direct/${process.env.API_VERSION}/users`);
}

export function directNullish(kind?: string) {
  return http.get(`/direct/${kind ?? "all"}`);
}

export function sameServiceFlag(archived: boolean) {
  return listThings(archived ? "archived" : "active");
}

export function sameServiceMode() {
  return listThings(MODE);
}

export function concatFlag(archived: boolean) {
  return http.get("/concat/" + (archived ? "archived" : "active") + "/x");
}

export function plusInside(a: string, b: string) {
  return http.get(`/plus/${a + b}/x`);
}
