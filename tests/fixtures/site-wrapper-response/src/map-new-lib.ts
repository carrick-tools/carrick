import { reply } from "@fixture/reply";
class User {
  constructor(public raw: { name: string }) {}
}
export async function user(p: string) {
  return new User((await reply.get<{ name: string }>(p)).data);
}
