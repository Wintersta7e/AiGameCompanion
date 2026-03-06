import App from "./App.svelte";
import { mount } from "svelte";
import "./app.css";

const el = document.getElementById("app");
if (!el) throw new Error("Missing #app element");

const app = mount(App, {
  target: el,
});

export default app;
