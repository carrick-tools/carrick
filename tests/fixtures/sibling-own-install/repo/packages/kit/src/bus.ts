export function postChained() {
  new BroadcastChannel('kit-bus').postMessage({ n: 1 });
}
