import { memberPlain, memberShadow } from "./member";

export async function useMembers() {
  await memberShadow(true);
  return memberPlain();
}
