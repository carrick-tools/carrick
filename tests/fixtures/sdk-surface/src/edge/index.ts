// Published only under the `./edge` subpath: nothing in the root entry
// re-exports it, so a consumer that imports the package root reaches none of
// these members.
export const edge = {
  async publish(id: string) {
    return fetch(`/v1/edge/publish/${id}`, { method: "POST" });
  },
};
