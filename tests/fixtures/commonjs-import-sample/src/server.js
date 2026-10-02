require("dotenv").config();
const express = require("express");
const { Kafka } = require("kafkajs");
const { publishOrder } = require("./publisher");

const app = express();
const kafka = new Kafka({ clientId: "orders", brokers: [process.env.KAFKA_BROKER] });

app.post("/orders", async (req, res) => {
  await publishOrder(kafka, req.body);
  res.status(202).json({ accepted: true });
});

module.exports = app;
