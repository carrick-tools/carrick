import { connect } from "@fixture/broker";

const client = connect();

client.subscribe("payments.settled");
