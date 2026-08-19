export const render = async (url: string): Promise<string> => {
  const { headless } = await import("crawler-kit");
  const page = await headless.launch(url);

  const runtime = await import("crawler-kit");
  await runtime.shutdown();

  return page;
};
