// A service that takes nothing from its caller.
export default async function init(): Promise<void> {
  await Promise.resolve();
}
