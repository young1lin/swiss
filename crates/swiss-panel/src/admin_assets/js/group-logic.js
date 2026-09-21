/*
 * Copyright 2026 young1lin
 * 
 * Licensed under the Apache License, Version 2.0 (the "License");
 * you may not use this file except in compliance with the License.
 * You may obtain a copy of the License at
 * 
 *     https://www.apache.org/licenses/LICENSE-2.0
 * 
 * Unless required by applicable law or agreed to in writing, software
 * distributed under the License is distributed on an "AS IS" BASIS,
 * WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 * See the License for the specific language governing permissions and
 * limitations under the License.
 */

/* ================================================================================================
   Group logic - the pure half of groups.js (docs/20 G4/G3).

   Data in, data out: no DOM, no localStorage, no fetch. That is the point - these rules are
   the contracts every scope shares (which group a row renders under, what the delete confirm
   promises, where a fresh row lands when nobody names a group), and a rule shared by six
   scopes is a rule worth pinning in a test. groups.js re-exports everything so callers keep
   one import; this file is where they live.
   ================================================================================================ */
                                                             
import { DEFAULT_GROUP } from "./util.js";
import { tr, trn } from "./i18n.js";

/* --- membership --------------------------------------------------------------------------------- */

/** The group a row renders under: its stored group while that group still exists, else the
 *  FIRST group - that slot is the sink for unassigned rows, whatever it is called (mirrors
 *  the server's one rule, docs/20 2.1). */
function groupOf(names          )                                                 {
  const first = names[0] || DEFAULT_GROUP;
  return (row) => {
    return row && row.group && names.includes(row.group) ? row.group : first;
  };
}

/** The list's shape: the groups in their stored order, each holding its members in the flat
 *  order the caller already sorted by. Empty groups keep their slot - you have to be able
 *  to see a group you just made. */
function slice     (rows       , names          , fn                      )                    {
  return names.map((name) => {
    return { name: name, rows: rows.filter((r) => { return fn(r) === name; }) };
  });
}

/* --- wording ------------------------------------------------------------------------------------ */

/** The delete confirm names where the members go: the FIRST group that remains - that slot is
 *  the server's sink. Tunnels once hard-coded 'default' here, which stayed wrong after the
 *  first group was renamed. Pure so the wording is pinned by tests, not by typing. */
function deleteConfirmMsg(name        , names          , count        , noun        )         {
  // The caller guarantees at least one OTHER group exists (delete is disabled on the last
  // one); the fallback keeps the type honest rather than asserting it.
  const sink = names.find((g) => { return g !== name; }) || name;
  return trn(count,
    "groupLogic.deleteGroupNameN.one",
    "groupLogic.deleteGroupNameN.other",
    { name, n: count, noun, sink });
}

/** A create title that says where the new thing goes ("New MCP in learn"). The group is
 *  part of the promise the + made; a sheet that opens unnamed breaks it. The old
 *  verb+noun composition ("Add an" + "MCP" + "to learn") could not survive translation
 *  (docs/38 L2: no concatenation in visible copy), so the sentence is one key. */
function addTitle(noun        , group        )         {
  return tr("groupLogic.newNounGroup", { noun, group });
}

/* --- the empty line ------------------------------------------------------------------------------ */

/** What an empty group's one quiet line says. The line exists to keep the container visible
 *  as a drop target, not to teach the whole flow - the head's + already does that - so it
 *  stays two words plus at most the drop affordance. Pure so the wording is pinned by tests
 *  like the delete confirm's is. */
function emptyLineText(canDrop         )         {
  return canDrop ? tr("groupLogic.itemsDropHerePress") : tr("groupLogic.items");
}

/* --- the last-used group ------------------------------------------------------------------------- */

/** The localStorage key for a scope's last-used group ("the one I picked last time I added
 *  something here"). Same prefix as the collapse keys, so a 'clear panel state' that sweeps
 * swiss.groups.* takes both. */
function lastGroupKey(scope        )         {
  return "swiss.groups." + scope + ".last";
}

/** Where a fresh row lands when the caller does not name a group (the top New button, the
 *  empty state): the group used last in this scope, while it still exists - a remembered
 *  name whose group was deleted or renamed is not a promise anybody made - else the first
 *  slot, the sink. Reading localStorage stays with the caller; this is the policy. */
function resolveDefaultGroup(names          , lastUsed                           )         {
  return lastUsed && names.includes(lastUsed) ? lastUsed : names[0] || DEFAULT_GROUP;
}

export { addTitle, deleteConfirmMsg, emptyLineText, groupOf, lastGroupKey, resolveDefaultGroup, slice };
