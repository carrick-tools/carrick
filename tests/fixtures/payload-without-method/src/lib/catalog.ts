export const catalog = {
  async search(query: string) {
    const response = await fetch(`/v1/catalog?q=${query}`, {
      headers: { accept: "application/json" },
    });
    return response.json();
  },
};
