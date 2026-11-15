import router from "fixture-router";
import env from "./env";
import services from "./services";

type Server = { close(): void };

// E: each requested service is loaded from the registry and handed the app.
export async function start(server: unknown): Promise<void> {
  const app = router();
  for (const name of env.SERVICES) {
    if (!Object.keys(services).includes(name)) {
      throw new Error(`no such service: ${name}`);
    }
    const { default: init } = await services[name as keyof typeof services]();
    await Promise.resolve(init(app, server as Server, env.SERVICES));
  }
}
