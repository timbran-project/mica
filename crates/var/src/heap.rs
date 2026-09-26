// Copyright (C) 2026 Ryan Daum <ryan.daum@gmail.com> This program is free
// software: you can redistribute it and/or modify it under the terms of the GNU
// Affero General Public License as published by the Free Software Foundation,
// version 3.
//
// This program is distributed in the hope that it will be useful, but WITHOUT
// ANY WARRANTY; without even the implied warranty of MERCHANTABILITY or FITNESS
// FOR A PARTICULAR PURPOSE. See the GNU Affero General Public License for more
// details.
//
// You should have received a copy of the GNU Affero General Public License along
// with this program. If not, see <https://www.gnu.org/licenses/>.

use crate::RelationValue;
use crate::string::HeapString;
use crate::value::{
    ErrorValue, FrobValue, TAG_BYTES, TAG_ERROR, TAG_FROB, TAG_LIST, TAG_MAP, TAG_RANGE,
    TAG_RELATION, TAG_STRING, Value,
};

pub(crate) enum HeapValue {
    String(HeapString),
    Bytes(Box<[u8]>),
    List(Box<[Value]>),
    Map(Box<[(Value, Value)]>),
    Relation(RelationValue),
    Range { start: Value, end: Option<Value> },
    Error(ErrorValue),
    Frob(FrobValue),
}

impl HeapValue {
    pub(crate) fn tag(&self) -> u8 {
        match self {
            Self::String(_) => TAG_STRING,
            Self::Bytes(_) => TAG_BYTES,
            Self::List(_) => TAG_LIST,
            Self::Map(_) => TAG_MAP,
            Self::Relation(_) => TAG_RELATION,
            Self::Range { .. } => TAG_RANGE,
            Self::Error(_) => TAG_ERROR,
            Self::Frob(_) => TAG_FROB,
        }
    }
}
