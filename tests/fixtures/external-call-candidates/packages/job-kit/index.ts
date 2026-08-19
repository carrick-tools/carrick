export const loadHandler = async () => {
  const { run } = await import("./handler");

  return run;
};
