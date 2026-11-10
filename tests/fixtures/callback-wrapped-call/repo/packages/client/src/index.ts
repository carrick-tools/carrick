async function send(url: string, init: { method: string }): Promise<Response> {
  return await fetch(url, init);
}

async function retried<T>(run: () => Promise<T>): Promise<T> {
  return await run();
}

export async function token(): Promise<string> {
  const res = await send("/auth/token", { method: "POST" });
  return res.text();
}

async function report(id: string): Promise<boolean> {
  return retried(async () => {
    const res = await send(`/reports/${id}`, { method: "POST" });
    return res.ok;
  });
}

// Two requests, one after the other, through one `send`.
export async function verify(id: string): Promise<boolean> {
  await token();
  return report(id);
}
