import { welcome } from "./invoke";

export async function onSignup(user: string) {
  return welcome(user);
}
