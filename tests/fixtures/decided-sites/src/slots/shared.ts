export function buildOptions(id: string) {
  return { key: [id], load: () => fetch(`/api/b/${id}`) };
}
