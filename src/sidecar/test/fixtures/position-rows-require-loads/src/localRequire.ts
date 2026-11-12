export async function functionLocal() {
  const { Client, Command } = require("sdk");
  const client = new Client();
  await client.send(new Command({ Name: "function-local" }));
}
