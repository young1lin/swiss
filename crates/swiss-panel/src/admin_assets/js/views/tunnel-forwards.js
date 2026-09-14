/* The Port Forwards page (#tunnel-forwards): the tunnels plugin's second L2 page, sibling
   of #tunnels. Same shape as views/tunnels.js on purpose — a page descriptor's entry module
   states its scope and nothing else; the shared module owns the rendering, so the two pages
   can never drift apart. */
import { mountTunnelsPage, pollTunnelsPage, refreshTunnelsPage, tunnelsCountText, unmountTunnelsPage } from "../tunnels.js";

export function mount() { return mountTunnelsPage("rules"); }
export function refresh() { return refreshTunnelsPage(); }
export function poll() { return pollTunnelsPage(); }
export function countText() { return tunnelsCountText("rules"); }
export function unmount() { unmountTunnelsPage(); }
