declare function deliver(path: string, body: unknown): Promise<void>;

export async function notify(body: unknown) {
  await deliver('/notify', body);
}
