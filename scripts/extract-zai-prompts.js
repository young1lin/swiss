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

const fs = require('fs');
const path = require('path');
// SRC: the unpacked npm package's build/prompts directory - first argument, else
// $ZAI_MCP_SERVER_DIR/build/prompts. Get one with `npm pack @z_ai/mcp-server@0.1.5` + untar.
// OUT: this repo's zai_prompts.rs, located relative to the script itself.
const srcRoot = process.argv[2] || (process.env.ZAI_MCP_SERVER_DIR && path.join(process.env.ZAI_MCP_SERVER_DIR, 'build', 'prompts'));
if (!srcRoot) { console.error('usage: node scripts/extract-zai-prompts.js <zai-mcp-server>/build/prompts'); process.exit(2); }
const SRC = srcRoot;
const OUT = path.join(__dirname, '..', 'crates', 'swiss-mcp', 'src', 'adapters', 'zai_prompts.rs');
// Unescape a JS template literal body: \` -> `, \${ -> ${, \\ -> \
function unescapeTpl(s) {
  let out = '';
  for (let i = 0; i < s.length; i++) {
    if (s[i] === '\\' && i + 1 < s.length) { const n = s[i+1];
      if (n === '`' || n === '$' || n === '\\') { out += n; i++; continue; } }
    out += s[i];
  }
  return out;
}
function rawDelim(text) {
  let n = 1;
  while (text.includes('"' + '#'.repeat(n))) n++;
  return n;
}
const MAP = {
  'data-viz.js': [['DATA_VIZ_ANALYSIS_PROMPT','DATA_VIZ']],
  'diagram-analysis.js': [['DIAGRAM_UNDERSTANDING_PROMPT','DIAGRAM']],
  'error-diagnosis.js': [['ERROR_DIAGNOSIS_PROMPT','ERROR_DIAGNOSIS']],
  'general-image.js': [['GENERAL_IMAGE_ANALYSIS_PROMPT','GENERAL_IMAGE']],
  'text-extraction.js': [['TEXT_EXTRACTION_PROMPT','TEXT_EXTRACTION']],
  'ui-diff.js': [['UI_DIFF_CHECK_PROMPT','UI_DIFF']],
};
let consts = [];
const manifest = [];
for (const [file, pairs] of Object.entries(MAP)) {
  const t = fs.readFileSync(path.join(SRC, file), 'utf8');
  for (const [jsName, rsName] of pairs) {
    const start = t.indexOf('const ' + jsName + ' = `');
    if (start < 0) throw new Error('no ' + jsName + ' in ' + file);
    const body0 = t.indexOf('`', start) + 1;
    const end = t.indexOf('`;', body0);
    const body = unescapeTpl(t.slice(body0, end));
    consts.push({ name: rsName, text: body });
    manifest.push({ src: jsName, rs: rsName, bytes: body.length, head: body.slice(0, 60).replace(/\n/g, ' ') });
  }
}
// ui-to-artifact: one object with four template keys
const t = fs.readFileSync(path.join(SRC, 'ui-to-artifact.js'), 'utf8');
for (const key of ['code','prompt','spec','description']) {
  const needle = key + ': `';
  const start = t.indexOf(needle);
  if (start < 0) throw new Error('no ' + key + ' in ui-to-artifact.js');
  const body0 = start + needle.length;
  const end = t.indexOf('`,', body0);
  const body = unescapeTpl(t.slice(body0, end));
  const rsName = 'UI_TO_ARTIFACT_' + key.toUpperCase();
  consts.push({ name: rsName, text: body });
  manifest.push({ src: 'UI_TO_ARTIFACT_PROMPTS.' + key, rs: rsName, bytes: body.length, head: body.slice(0, 60).replace(/\n/g, ' ') });
}
let rs = '//! System prompts for the zai-vision adapter, ported VERBATIM from @z_ai/mcp-server\n';
rs += '//! 0.1.5 (build/prompts/*.js). The text is the product — every prompt survived the move\n';
rs += '//! byte for byte, extracted by a one-shot script rather than retyped. Do not edit by hand;\n';
rs += '//! re-extract with scripts/extract-zai-prompts.js from an unpacked copy of the npm package\n';
rs += '//! (`npm pack @z_ai/mcp-server@0.1.5`) if upstream moves. Attribution: THIRD_PARTY_NOTICES.md.\n\n';
for (const c of consts) {
  const d = rawDelim(c.text);
  const open = 'r' + '#'.repeat(d) + '"';
  const close = '"' + '#'.repeat(d);
  rs += 'pub(crate) const ' + c.name + ' : &str = ' + open + c.text + close + ';\n\n';
}
fs.writeFileSync(OUT, rs.replace(/\r\n/g, '\n'));
console.log(JSON.stringify(manifest, null, 1));