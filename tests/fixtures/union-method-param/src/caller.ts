import { archive, touch } from "./members";

export async function remove(id: string) {
  await archive("DELETE", id);
}

export async function poke(id: string, verb: "POST" | "DELETE") {
  await touch("POST", id);
  await touch(verb, id);
}
