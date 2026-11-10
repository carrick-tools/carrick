export const handler = async (event: { body: string }) => {
  const body = JSON.parse(event.body);
  switch (body.kind) {
    case "a":
      return doA(body);
    case "b":
      return doB(body);
    case "c":
      return doC(body);
    case "d":
      return { statusCode: 200, body: "{}" };
  }
};

function doA(_body: unknown) {
  return { statusCode: 200, body: "{}" };
}
function doB(_body: unknown) {
  return { statusCode: 200, body: "{}" };
}
function doC(_body: unknown) {
  return { statusCode: 200, body: "{}" };
}
