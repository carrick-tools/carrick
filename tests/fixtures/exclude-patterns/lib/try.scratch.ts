export async function tryLegacy() {
  const LEGACY_URL = "/api/legacy";
  return fetch(LEGACY_URL);
}
