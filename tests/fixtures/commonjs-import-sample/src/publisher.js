async function publishOrder(kafka, order) {
  const producer = kafka.producer();
  await producer.connect();
  await producer.send({
    topic: "orders.created",
    messages: [{ value: JSON.stringify(order) }],
  });
}

async function orderCache() {
  const { default: Redis } = await import("ioredis");
  return new Redis(process.env.REDIS_URL);
}

function logger(name) {
  return require("pino")({ name, level: process.env.LOG_LEVEL });
}

function plugin(name) {
  return require(`./plugins/${name}`);
}

module.exports = { publishOrder, orderCache, logger, plugin };
