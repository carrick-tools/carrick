import { redirect } from "a-server-runtime";
import { commitSession, readSession } from "./session.server";

const SETTINGS_PATH = "/settings/security";
const LOGIN_PATH = "/login";

export async function backWithError(request: Request, message: string) {
  const session = await readSession(request.headers.get("cookie"));
  session.flash("error", message);
  return redirect(SETTINGS_PATH, {
    headers: { "Set-Cookie": await commitSession(session) },
  });
}

export async function requireSignedIn(request: Request, headers: Headers) {
  if (!request.headers.get("cookie")) {
    throw redirect(LOGIN_PATH, { status: 303, headers });
  }
}
