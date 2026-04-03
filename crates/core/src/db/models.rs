use serde::{Deserialize, Serialize};
use std::collections::HashMap;

pub const FM: u8 = 254; // Field Mark
pub const VM: u8 = 253; // Value Mark
pub const SVM: u8 = 252; // Sub-Value Mark

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Record {
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Field {
    pub values: Vec<Value>,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Value {
    pub sub_values: Vec<String>,
}

impl Record {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn from_bytes(data: &[u8]) -> Self {
        if data.is_empty() {
            return Record { fields: vec![] };
        }
        let fields = data.split(|&b| b == FM)
            .map(|f| {
                let values = f.split(|&b| b == VM)
                    .map(|v| {
                        let sub_values = v.split(|&b| b == SVM)
                            .map(|sv| String::from_utf8_lossy(sv).to_string())
                            .collect();
                        Value { sub_values }
                    })
                    .collect();
                Field { values }
            })
            .collect();
        Record { fields }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let mut res = Vec::new();
        for (i, f) in self.fields.iter().enumerate() {
            if i > 0 { res.push(FM); }
            for (j, v) in f.values.iter().enumerate() {
                if j > 0 { res.push(VM); }
                for (k, sv) in v.sub_values.iter().enumerate() {
                    if k > 0 { res.push(SVM); }
                    res.extend_from_slice(sv.as_bytes());
                }
            }
        }
        res
    }

    pub fn to_display_string(&self) -> String {
        let display_bytes: Vec<u8> = self.to_bytes().iter().map(|&b| match b {
            FM => b'^',
            VM => b']',
            SVM => b'\\',
            _ => b
        }).collect();
        String::from_utf8_lossy(&display_bytes).to_string()
    }

    pub fn from_display_string(s: &str) -> Self {
        let translated_data: Vec<u8> = s.as_bytes().iter().map(|&b| match b {
            b'^' => FM,
            b']' => VM,
            b'\\' => SVM,
            _ => b
        }).collect();
        Self::from_bytes(&translated_data)
    }

    pub fn to_edit_string(&self) -> String {
        let display_bytes: Vec<u8> = self.to_bytes().iter().map(|&b| match b {
            FM => b'\n',
            VM => b']',
            SVM => b'\\',
            _ => b
        }).collect();
        String::from_utf8_lossy(&display_bytes).to_string()
    }

    pub fn from_edit_string(s: &str) -> Self {
        let mut content = s;
        if content.ends_with('\n') {
            content = &content[..content.len() - 1];
        }
        let translated_data: Vec<u8> = content.as_bytes().iter().map(|&b| match b {
            b'\n' => FM,
            b']' => VM,
            b'\\' => SVM,
            _ => b
        }).collect();
        Self::from_bytes(&translated_data)
    }

    pub fn get_field_display_string(&self, field_idx: usize) -> String {
        if let Some(field) = self.fields.get(field_idx) {
            let mut res = Vec::new();
            for (j, v) in field.values.iter().enumerate() {
                if j > 0 { res.push(VM); }
                for (k, sv) in v.sub_values.iter().enumerate() {
                    if k > 0 { res.push(SVM); }
                    res.extend_from_slice(sv.as_bytes());
                }
            }
            let display_bytes: Vec<u8> = res.iter().map(|&b| match b {
                FM => b'^',
                VM => b']',
                SVM => b'\\',
                _ => b
            }).collect();
            String::from_utf8_lossy(&display_bytes).to_string()
        } else {
            String::new()
        }
    }
}

pub struct Table {
    pub records: HashMap<String, Record>,
    pub dictionary: HashMap<String, Record>,
    pub dirty: bool,
}

impl Table {
    pub fn new() -> Self {
        Table {
            records: HashMap::new(),
            dictionary: HashMap::new(),
            dirty: false,
        }
    }
}

#[derive(Clone, Debug)]
pub struct SelectList {
    pub table_name: String,
    pub is_dict: bool,
    pub keys: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ClientInfo {
    pub thumbprint: String,
    pub allowed_accounts: Vec<String>,
    pub is_admin: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct QueryCondition {
    pub field_name: String,
    pub op: String,
    pub value: String,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum LogicalOp {
    And,
    Or,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum QueryNode {
    Condition(QueryCondition),
    Logical {
        op: LogicalOp,
        left: Box<QueryNode>,
        right: Box<QueryNode>,
    },
}
