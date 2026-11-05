export const syncCatalog = (items: unknown[]) => {
  return fetch("/catalog/sync", {
    method: "POST",
    body: JSON.stringify(items),
  });
};
