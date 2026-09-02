// A second binding whose method name collides with the client's. It issues no
// request, and it is imported from here rather than from ./apiClient, so a
// site that calls through it must not take the client's URL.
export const apiClient = {
  createArtifactUrl(name: string) {
    return `local:${name}`;
  },
};
