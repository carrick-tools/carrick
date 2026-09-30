import { profile } from "./lib/profile.js";

export function saveProfile(name: string) {
  return profile.save({ name });
}
