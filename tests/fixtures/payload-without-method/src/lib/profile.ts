import { client } from "./client.js";
import { useForm } from "./form.js";

export const profile = {
  save(values: { name: string }) {
    const form = useForm({ data: values, validateOn: "submit" });
    return client.updateProfile(form.values);
  },
};
