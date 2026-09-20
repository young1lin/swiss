/*
 * Copyright 2026 The swiss authors
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

/* The Chinese dictionary (docs/38 L3): one flat table, English source strings as keys,
   section comments naming the area the keys below belong to. A plural's entry is the
   "other" form only — Chinese has no "one" category under Intl.PluralRules("zh-CN"), so
   trn() never looks for one. Values carry {name} placeholders exactly where the English
   key does. The completeness scanner (test/i18n-complete.test.ts) holds both directions
   honest: every key used, no orphan entries, no value equal to its key. */

const zh: Record<string, string> = {
  /* --- chrome (index.html, main.ts) --- */
  "Search": "搜索",
  "Filter MCPs": "筛选 MCP",
  "New group": "新建分组",
  "Appearance": "外观",
  "Switch to dark": "切换到深色",
  "Switch to light": "切换到浅色",
  "Focus mode — hide app navigation (Esc exits)": "专注模式——隐藏应用导航(Esc 退出)",
  "Focus mode": "专注模式",
  "Language": "语言",
  "MCPs": "MCP",
  "Hosted MCPs": "托管的 MCP",
  "Plugins": "插件",
  "swiss resident set": "swiss 常驻内存",
  "mem …": "内存 …",
  "A new panel version is ready — it will load once you finish editing": "面板新版本已就绪——完成编辑后会自动加载",
};

export default zh;
