type Handler = (body: string) => Promise<void>;

declare const handlers: Record<string, Handler>;

export async function dispatch(type: string, body: string) {
  await handlers[type](body);
}
