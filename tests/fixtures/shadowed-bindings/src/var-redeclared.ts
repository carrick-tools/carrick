export async function varRedeclared(flag: boolean) {
  var path = "/api/var-first";
  if (flag) {
    var path = "/api/var-second";
  }
  return fetch(path, { method: "POST" });
}
