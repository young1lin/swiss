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

//! Fixtures the data browser's unit tests share. The tests were ported from the Node build's
//! dbbrowser.test.ts (the pure-helper half; the HTTP half lives in dbbrowser_api's tests).

use serde_json::{Map, Value};

use super::edit::browse_edit_of;
use super::filter::browse_filters_of;
use super::model::{BrowseColumn, BrowseEdit, BrowseFilter, FilterColumn};

pub(super) fn names(cols: &[&str]) -> Vec<FilterColumn> {
    cols.iter()
        .map(|c| FilterColumn::Name((*c).to_string()))
        .collect()
}

pub(super) fn col(name: &str, data_type: &str, nullable: bool, pk: bool) -> BrowseColumn {
    BrowseColumn {
        name: name.into(),
        data_type: data_type.into(),
        nullable,
        is_primary_key: pk,
        default_value: None,
        comment: None,
    }
}

pub(super) fn filter(json: Value) -> Vec<BrowseFilter> {
    browse_filters_of(Some(&json))
}

pub(super) fn stub_columns() -> Vec<BrowseColumn> {
    vec![
        col("id", "int", false, true),
        col("name", "text", true, false),
    ]
}

pub(super) fn edit(v: Value) -> BrowseEdit {
    browse_edit_of(&v).unwrap()
}

pub(super) fn row(pairs: &[(&str, Value)]) -> Map<String, Value> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect()
}
