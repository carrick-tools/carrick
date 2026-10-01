import * as api from "./member";

export async function useMembersThroughNamespace() {
  await api.memberShadow(true);
  return api.memberPlain();
}
