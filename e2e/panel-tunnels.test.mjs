// Tunnels views, real browser: both sibling pages (SSH Connections, Port
// Forwards) open visibly, errorless.
import { withPanel } from "./browser.mjs";

await withPanel(async (b) => {
  for (const id of ["tunnels", "tunnel-forwards"]) {
    await b.navigate("http://127.0.0.1:" + (process.env.SWISS_E2E_PORT || "18999") + "/#" + id);
    await b.waitFor("(() => location.hash === '#" + id + "')()");
    await b.waitFor("(() => (document.querySelector('#pane') || {}).childElementCount > 0)()");
    await b.visibleOk("#pane > *");
  }
  console.log("panel-tunnels: ok");
}, { plugin: "tunnels", view: "#tunnels" });