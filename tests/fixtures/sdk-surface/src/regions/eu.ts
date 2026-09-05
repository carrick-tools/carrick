// Reached only through the `./regions/*` pattern key, which names no single
// module: the surface walks no entry for it, and a consumer importing
// `@fixture/ledger-sdk/regions/eu` resolves to no member.
export const eu = {
  async ping() {
    return fetch("/v1/regions/eu/ping");
  },
};
