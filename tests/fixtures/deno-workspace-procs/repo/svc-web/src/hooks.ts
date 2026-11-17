import { createTRPCReact } from '@trpc/react-query'
import type { ShopRouter } from '../../svc-api/src/rpc.ts'

export const rpc = createTRPCReact<ShopRouter>({})

export function usePlaceOrder() {
  const placeMutation = rpc.place.useMutation({
    onSuccess: () => {},
  })

  return (sku: string) => placeMutation.mutateAsync({ sku })
}
