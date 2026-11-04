import express from "express";

interface EventBody {
  name: string;
  at: number;
}

const app = express();
app.use(express.json());

app.post("/events", (req, res) => {
  const body: EventBody = req.body;
  console.log(body.name, body.at);
  res.status(204).end();
});

app.listen(4200);
