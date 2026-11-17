import { initTRPC } from '@trpc/server'

const t = initTRPC.create()

export const router = t.router
export const proc = t.procedure

const skuInput = (value: unknown) => value as { sku: string }

export const shopRouter = router({
  ping: proc.query(() => {
    return { ok: true }
  }),

  place: proc.input(skuInput).mutation(({ input }) => {
    return { id: input.sku }
  }),
})

export type ShopRouter = typeof shopRouter
