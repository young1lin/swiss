import { state } from "../util.js";
import { dbConnLabel, dbOkToDrop, dbPending, loadDbView } from "../data-view.js";
export function mount() { return loadDbView(); }
export function refresh() { return loadDbView(); }
export function hasPendingChanges() { return dbPending() > 0; }
export function canLeave() { return !dbPending() || dbOkToDrop(); }
export function unmount() { state.db = null; }
/* The page bar's count chip. It used to return "Data", which the group label one slot left
   already says — the bar read "Data … Data". It names the connection being browsed instead,
   in the dropdown's own words (dbConnLabel, one builder for both), and renders nothing when
   no connection is selected rather than repeating the page name. */
export function countText() {
  var d = state.db;
  if (!d || !d.conn) return "";
  var c = (d.conns || []).find(function (x) { return x.name === d.conn; });
  return c ? dbConnLabel(c) : "";
}
