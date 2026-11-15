import { listThings } from "@fixture/api";

declare const user: { kind: string };
declare const n: number;

export async function byMember() {
  return listThings(user.kind);
}

export async function byString() {
  return listThings(String(n));
}

export async function byEnv() {
  return listThings(process.env.KIND as string);
}

export async function byNullish(k?: string) {
  return listThings(k ?? "all");
}
