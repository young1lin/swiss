import { state } from "../util.js";
import { dbOkToDrop, dbPending, loadDbView } from "../data-view.js";
export function mount() { return loadDbView(); }
export function refresh() { return loadDbView(); }
export function hasPendingChanges() { return dbPending() > 0; }
export function canLeave() { return !dbPending() || dbOkToDrop(); }
export function unmount() { state.db = null; }
export function countText() { return "Data"; }
