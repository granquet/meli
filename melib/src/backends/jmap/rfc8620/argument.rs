/*
 * meli - jmap module.
 *
 * Copyright 2019 Manos Pitsidianakis
 *
 * This file is part of meli.
 *
 * meli is free software: you can redistribute it and/or modify
 * it under the terms of the GNU General Public License as published by
 * the Free Software Foundation, either version 3 of the License, or
 * (at your option) any later version.
 *
 * meli is distributed in the hope that it will be useful,
 * but WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
 * GNU General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with meli. If not, see <http://www.gnu.org/licenses/>.
 */

use crate::backends::jmap::{
    protocol::Method,
    rfc8620::{Object, ResultField},
};

#[derive(Deserialize, Serialize, Debug)]
#[serde(rename_all = "camelCase")]
pub enum JmapArgument<T> {
    Value(T),
    ResultReference {
        result_of: String,
        name: String,
        path: String,
    },
}

impl<T> JmapArgument<T> {
    pub fn value(v: T) -> Self {
        Self::Value(v)
    }

    pub fn reference<M, OBJ>(result_of: usize, path: ResultField<M, OBJ>) -> Self
    where
        M: Method<OBJ>,
        OBJ: Object,
    {
        Self::ResultReference {
            result_of: format!("m{}", result_of),
            name: M::NAME.to_string(),
            path: path.field.to_string(),
        }
    }
}
