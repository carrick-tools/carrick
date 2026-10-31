// Gives the scan one file the model is asked about.
export async function loadOrders() {
  return fetch('/orders');
}
