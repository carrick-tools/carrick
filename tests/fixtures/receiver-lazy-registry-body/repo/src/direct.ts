import router from "fixture-router";

// F: a destructured key of a module loaded directly in a function body, no
// registry between. Out of this ticket's scope: it stays as it reads today.
export async function startF(): Promise<void> {
  const app = router();
  const { default: init } = await import("./svc/f");
  init(app);
}
