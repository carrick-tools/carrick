import { Hono } from 'hono'
import { trpcServer } from '@hono/trpc-server'
import { shopRouter } from '@/rpc.ts'

function buildApp() {
  const app = new Hono()

  app.use(
    '/rpc/*',
    trpcServer({
      router: shopRouter,
      createContext: () => ({}),
    }),
  )

  return app
}

Deno.serve(buildApp().fetch)
