// Requests one existing static file, one missing static file and one route.
export async function preload(): Promise<void> {
  await fetch("/images/banner.svg");
  await fetch("/images/missing.svg");
  await fetch("/api/items");
}
