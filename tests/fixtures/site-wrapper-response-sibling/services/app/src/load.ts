import { clearAs, clearWhole } from "@fixture/net/clear";
function loadGone(path: string) {
  return clearAs<{ gone: boolean }>(path);
}
function loadWiped(path: string) {
  return clearWhole(path);
}
export const gone = () => loadGone("/bins");
export const wiped = () => loadWiped("/wipes");
