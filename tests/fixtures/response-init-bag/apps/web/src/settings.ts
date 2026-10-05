import { backWithError, requireSignedIn } from "./security.server";

export async function action(request: Request) {
  await requireSignedIn(request, new Headers());
  const form = await request.formData();
  if (!form.get("password")) {
    return backWithError(request, "A password is required.");
  }
  return new Response(null, { status: 204 });
}
