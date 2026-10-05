export interface Session {
  flash(key: string, value: string): void;
}

const sessions = new Map<string, Session>();

export async function readSession(cookie: string | null): Promise<Session> {
  const existing = sessions.get(cookie ?? "");
  if (existing) return existing;
  const created: Session = { flash: () => undefined };
  sessions.set(cookie ?? "", created);
  return created;
}

export async function commitSession(session: Session): Promise<string> {
  return sessions.size > 0 && session ? "session=1" : "";
}
