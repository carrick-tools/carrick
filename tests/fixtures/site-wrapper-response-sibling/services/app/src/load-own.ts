import { clearOwnAs, clearOwnWhole } from "./clear-own";
function loadOwnGone(path: string) {
  return clearOwnAs<{ gone: boolean }>(path);
}
function loadOwnWiped(path: string) {
  return clearOwnWhole(path);
}
export const ownGone = () => loadOwnGone("/own-bins");
export const ownWiped = () => loadOwnWiped("/own-wipes");
