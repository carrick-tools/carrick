// Nightly cache warm: reads every stock level once, so the first requests of
// the day are answered from memory. Runs inside this service and goes through
// its own HTTP surface over localhost.
export async function warmStockCache(warehouseIds: string[], skus: string[]): Promise<void> {
  for (const wid of warehouseIds) {
    for (const sku of skus) {
      await fetch(`http://localhost:4002/warehouses/${wid}/stock/${sku}`);
    }
  }
}
