export async function newUrlFrame(flag: boolean) {
  const link = new URL("/api/frame-outer", process.env.SERVICE_URL);
  if (flag) {
    const link = new URL("/api/frame-inner", process.env.SERVICE_URL);
    console.log(link.href);
  }
  return fetch(link, { method: "POST" });
}
