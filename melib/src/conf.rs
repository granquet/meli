/*
 * meli - configuration module.
 *
 * Copyright 2017 Manos Pitsidianakis
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

//! Basic mail account configuration to use with [`backends`](./backends/index.html)
use crate::backends::SpecialUsageMailbox;
pub use crate::{SortField, SortOrder};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::collections::HashMap;

#[derive(Debug, Serialize, Default, Clone)]
pub struct AccountSettings {
    pub name: String,
    pub root_mailbox: String,
    pub format: String,
    pub identity: String,
    pub extra_identities: Vec<String>,
    pub read_only: bool,
    pub display_name: Option<String>,
    #[serde(default)]
    pub order: (SortField, SortOrder),
    pub subscribed_mailboxes: Vec<String>,
    #[serde(default)]
    pub mailboxes: HashMap<String, MailboxConf>,
    #[serde(default)]
    pub manual_refresh: bool,
    #[serde(flatten)]
    pub extra: HashMap<String, String>,
}

impl AccountSettings {
    pub fn format(&self) -> &str {
        &self.format
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn set_name(&mut self, s: String) {
        self.name = s;
    }
    pub fn root_mailbox(&self) -> &str {
        &self.root_mailbox
    }
    pub fn identity(&self) -> &str {
        &self.identity
    }
    pub fn read_only(&self) -> bool {
        self.read_only
    }
    pub fn display_name(&self) -> Option<&String> {
        self.display_name.as_ref()
    }
    pub fn order(&self) -> Option<(SortField, SortOrder)> {
        Some(self.order)
    }

    pub fn subscribed_mailboxes(&self) -> &Vec<String> {
        &self.subscribed_mailboxes
    }

    #[cfg(feature = "vcard")]
    pub fn vcard_folder(&self) -> Option<&str> {
        self.extra.get("vcard_folder").map(String::as_str)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MailboxConf {
    #[serde(alias = "rename")]
    pub alias: Option<String>,
    #[serde(default = "false_val")]
    pub autoload: bool,
    #[serde(default)]
    pub subscribe: ToggleFlag,
    #[serde(default)]
    pub ignore: ToggleFlag,
    #[serde(default = "none")]
    pub usage: Option<SpecialUsageMailbox>,
    #[serde(default = "none")]
    pub sort_order: Option<usize>,
    #[serde(flatten)]
    pub extra: HashMap<String, String>,
}

impl Default for MailboxConf {
    fn default() -> Self {
        MailboxConf {
            alias: None,
            autoload: false,
            subscribe: ToggleFlag::Unset,
            ignore: ToggleFlag::Unset,
            usage: None,
            sort_order: None,
            extra: HashMap::default(),
        }
    }
}

impl MailboxConf {
    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
}

pub fn true_val() -> bool {
    true
}

pub fn false_val() -> bool {
    false
}

pub fn none<T>() -> Option<T> {
    None
}

macro_rules! named_unit_variant {
    ($variant:ident) => {
        pub mod $variant {
            /*
            pub fn serialize<S>(serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(stringify!($variant))
            }
            */

            pub fn deserialize<'de, D>(deserializer: D) -> Result<(), D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct V;
                impl<'de> serde::de::Visitor<'de> for V {
                    type Value = ();
                    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                        f.write_str(concat!("\"", stringify!($variant), "\""))
                    }
                    fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
                        if value == stringify!($variant) {
                            Ok(())
                        } else {
                            Err(E::invalid_value(serde::de::Unexpected::Str(value), &self))
                        }
                    }
                }
                deserializer.deserialize_str(V)
            }
        }
    };
}

mod strings {
    named_unit_variant!(ask);
}

#[derive(Copy, Debug, Clone, PartialEq)]
pub enum ToggleFlag {
    Unset,
    InternalVal(bool),
    False,
    True,
    Ask,
}

impl From<bool> for ToggleFlag {
    fn from(val: bool) -> Self {
        if val {
            ToggleFlag::True
        } else {
            ToggleFlag::False
        }
    }
}

impl Default for ToggleFlag {
    fn default() -> Self {
        ToggleFlag::Unset
    }
}

impl ToggleFlag {
    pub fn is_unset(&self) -> bool {
        ToggleFlag::Unset == *self
    }
    pub fn is_internal(&self) -> bool {
        if let ToggleFlag::InternalVal(_) = *self {
            true
        } else {
            false
        }
    }

    pub fn is_ask(&self) -> bool {
        *self == ToggleFlag::Ask
    }

    pub fn is_false(&self) -> bool {
        ToggleFlag::False == *self || ToggleFlag::InternalVal(false) == *self
    }

    pub fn is_true(&self) -> bool {
        ToggleFlag::True == *self || ToggleFlag::InternalVal(true) == *self
    }
}

impl Serialize for ToggleFlag {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            ToggleFlag::Unset | ToggleFlag::InternalVal(_) => serializer.serialize_none(),
            ToggleFlag::False => serializer.serialize_bool(false),
            ToggleFlag::True => serializer.serialize_bool(true),
            ToggleFlag::Ask => serializer.serialize_str("ask"),
        }
    }
}

impl<'de> Deserialize<'de> for ToggleFlag {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        pub enum InnerToggleFlag {
            Bool(bool),
            #[serde(with = "strings::ask")]
            Ask,
        }
        let s = <InnerToggleFlag>::deserialize(deserializer);
        Ok(
            match s.map_err(|err| {
                serde::de::Error::custom(format!(
                    r#"expected one of "true", "false", "ask", found `{}`"#,
                    err
                ))
            })? {
                InnerToggleFlag::Bool(true) => ToggleFlag::True,
                InnerToggleFlag::Bool(false) => ToggleFlag::False,
                InnerToggleFlag::Ask => ToggleFlag::Ask,
            },
        )
    }
}
