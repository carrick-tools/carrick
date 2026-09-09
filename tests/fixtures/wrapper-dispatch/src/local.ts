const base = process.env.GATEWAY_URL ?? "";

async function postToGateway(path: string, payload: unknown) {
  return fetch(`${base}${path}`, {
    method: "POST",
    body: JSON.stringify({ action: "store-metadata", payload }),
  });
}

export async function storeMetadata(payload: unknown) {
  return postToGateway("/rpc/gateway", payload);
}
