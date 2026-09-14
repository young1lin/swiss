/* The SSH Connections page (#tunnels): one of the tunnels plugin's two L2 pages. A thin
   module by design — all data, rendering and actions live in ../tunnels.js and ../polling.js;
   this file only states which scope the page mounts, so the Context Bar's page switcher and
   the deep link #tunnels (kept for saved links) land on connections. */
import { mountTunnelsPage, pollTunnelsPage, refreshTunnelsPage, tunnelsCountText, unmountTunnelsPage } from "../tunnels.js";

export function mount() { return mountTunnelsPage("conns"); }
export function refresh() { return refreshTunnelsPage(); }
export function poll() { return pollTunnelsPage(); }
export function countText() { return tunnelsCountText("conns"); }
export function unmount() { unmountTunnelsPage(); }
