const { Client, Command } = require("sdk");
const client = new Client();
export async function moduleLevel() {
  await client.send(new Command({ Name: "module-level" }));
}
