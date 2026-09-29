//! Structural diffs for Soroban contract storage entries.

use crate::types::trace::{DiffChangeType, LedgerEntryDiff, StateDiff};
use serde::Serialize;
use std::fmt::Debug;
use stellar_xdr::curr::{ContractDataEntry, ScMap, ScVal};

/// Compute the changes to a contract storage entry's value.
///
/// Maps are compared by their XDR keys, vectors by index, and scalar values
/// directly. Each changed path is reported as a separate state-diff entry.
/// Unchanged values are omitted.
pub fn diff_contract_data(old: &ContractDataEntry, new: &ContractDataEntry) -> StateDiff {
    let mut entries = Vec::new();
    let root = format!("storage[{}]", json_string(&old.key));
    diff_value(&old.val, &new.val, &root, &mut entries);
    StateDiff { entries }
}

fn diff_value(old: &ScVal, new: &ScVal, path: &str, entries: &mut Vec<LedgerEntryDiff>) {
    match (old, new) {
        (ScVal::Map(Some(old_map)), ScVal::Map(Some(new_map))) => {
            diff_map(old_map, new_map, path, entries);
        }
        (ScVal::Vec(Some(old_vec)), ScVal::Vec(Some(new_vec))) => {
            let shared_len = old_vec.len().min(new_vec.len());
            for (index, (old_value, new_value)) in old_vec
                .iter()
                .zip(new_vec.iter())
                .take(shared_len)
                .enumerate()
            {
                diff_value(old_value, new_value, &format!("{path}[{index}]"), entries);
            }

            for (index, value) in old_vec.iter().enumerate().skip(shared_len) {
                push_change(
                    entries,
                    format!("{path}[{index}]"),
                    Some(value),
                    None,
                    DiffChangeType::Deleted,
                );
            }
            for (index, value) in new_vec.iter().enumerate().skip(shared_len) {
                push_change(
                    entries,
                    format!("{path}[{index}]"),
                    None,
                    Some(value),
                    DiffChangeType::Created,
                );
            }
        }
        _ if old != new => push_change(
            entries,
            path.to_owned(),
            Some(old),
            Some(new),
            DiffChangeType::Updated,
        ),
        _ => {}
    }
}

fn diff_map(old: &ScMap, new: &ScMap, path: &str, entries: &mut Vec<LedgerEntryDiff>) {
    let mut matched_new = vec![false; new.len()];

    for old_entry in old.iter() {
        if let Some((index, new_entry)) = new
            .iter()
            .enumerate()
            .find(|(_, candidate)| candidate.key == old_entry.key)
        {
            matched_new[index] = true;
            diff_value(
                &old_entry.val,
                &new_entry.val,
                &format!("{path}[{}]", json_string(&old_entry.key)),
                entries,
            );
        } else {
            push_change(
                entries,
                format!("{path}[{}]", json_string(&old_entry.key)),
                Some(&old_entry.val),
                None,
                DiffChangeType::Deleted,
            );
        }
    }

    for (index, new_entry) in new.iter().enumerate() {
        if !matched_new[index] {
            push_change(
                entries,
                format!("{path}[{}]", json_string(&new_entry.key)),
                None,
                Some(&new_entry.val),
                DiffChangeType::Created,
            );
        }
    }
}

fn push_change(
    entries: &mut Vec<LedgerEntryDiff>,
    key: String,
    before: Option<&ScVal>,
    after: Option<&ScVal>,
    change_type: DiffChangeType,
) {
    entries.push(LedgerEntryDiff {
        key,
        before: before.map(json_string),
        after: after.map(json_string),
        change_type,
    });
}

fn json_string<T: Debug + Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| format!("{value:?}"))
}

#[cfg(test)]
mod tests {
    use super::diff_contract_data;
    use crate::types::trace::DiffChangeType;
    use stellar_xdr::curr::{
        ContractDataDurability, ContractDataEntry, ExtensionPoint, Hash, ScMapEntry, ScSymbol,
        ScVal, StringM,
    };

    fn contract_data(val: ScVal) -> ContractDataEntry {
        ContractDataEntry {
            ext: ExtensionPoint::V0,
            contract: stellar_xdr::curr::ScAddress::Contract(Hash([0; 32])),
            key: ScVal::Symbol(ScSymbol(StringM::try_from("state").expect("symbol"))),
            durability: ContractDataDurability::Persistent,
            val,
        }
    }

    fn symbol(value: &str) -> ScVal {
        ScVal::Symbol(ScSymbol(StringM::try_from(value).expect("symbol")))
    }

    #[test]
    fn deeply_reports_added_deleted_and_modified_map_values() {
        let old = contract_data(ScVal::Map(Some(
            vec![ScMapEntry {
                key: symbol("account"),
                val: ScVal::Map(Some(
                    vec![
                        ScMapEntry {
                            key: symbol("balance"),
                            val: ScVal::I64(10),
                        },
                        ScMapEntry {
                            key: symbol("obsolete"),
                            val: ScVal::Bool(true),
                        },
                    ]
                    .try_into()
                    .expect("nested map"),
                )),
            }]
            .try_into()
            .expect("map"),
        )));
        let new = contract_data(ScVal::Map(Some(
            vec![ScMapEntry {
                key: symbol("account"),
                val: ScVal::Map(Some(
                    vec![
                        ScMapEntry {
                            key: symbol("balance"),
                            val: ScVal::I64(25),
                        },
                        ScMapEntry {
                            key: symbol("active"),
                            val: ScVal::Bool(true),
                        },
                    ]
                    .try_into()
                    .expect("nested map"),
                )),
            }]
            .try_into()
            .expect("map"),
        )));

        let diff = diff_contract_data(&old, &new);

        assert_eq!(diff.entries.len(), 3);
        assert!(matches!(
            diff.entries[0].change_type,
            DiffChangeType::Updated
        ));
        assert_eq!(diff.entries[0].before.as_deref(), Some(r#"{"i64":10}"#));
        assert_eq!(diff.entries[0].after.as_deref(), Some(r#"{"i64":25}"#));
        assert!(matches!(
            diff.entries[1].change_type,
            DiffChangeType::Deleted
        ));
        assert!(matches!(
            diff.entries[2].change_type,
            DiffChangeType::Created
        ));
        assert!(diff.entries[0].key.contains("balance"));
        assert!(diff.entries[1].key.contains("obsolete"));
        assert!(diff.entries[2].key.contains("active"));
    }

    #[test]
    fn equal_values_produce_no_changes() {
        let entry = contract_data(ScVal::Vec(Some(
            vec![ScVal::U32(7)].try_into().expect("vector"),
        )));

        assert!(diff_contract_data(&entry, &entry).entries.is_empty());
    }

    #[test]
    fn vector_length_changes_are_reported_by_index() {
        let old = contract_data(ScVal::Vec(Some(
            vec![ScVal::U32(1), ScVal::U32(2)]
                .try_into()
                .expect("vector"),
        )));
        let new = contract_data(ScVal::Vec(Some(
            vec![ScVal::U32(1), ScVal::U32(2), ScVal::U32(3)]
                .try_into()
                .expect("vector"),
        )));

        let diff = diff_contract_data(&old, &new);

        assert_eq!(diff.entries.len(), 1);
        assert!(matches!(
            diff.entries[0].change_type,
            DiffChangeType::Created
        ));
        assert!(diff.entries[0].key.ends_with("[2]"));
    }
}
