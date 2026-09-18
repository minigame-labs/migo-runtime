//! The numbers service ops travel under.
//!
//! `contracts/runtime/service-ops.json` is the source; this is the host's copy,
//! held to it by the test below. The producer's copy is generated from the same
//! file when the engine is staged. Append only -- see the contract.

macro_rules! service_ops {
    ($($name:ident = $id:literal,)*) => {
        /// One constant per op, named as the op is.
        #[allow(non_upper_case_globals)]
        pub(crate) mod id {
            $(pub(crate) const $name: u32 = $id;)*
        }

        /// Every op and its number, for the contract test and for naming an op
        /// in a refusal.
        pub(crate) const ALL: &[(&str, u32)] = &[$((stringify!($name), $id),)*];
    };
}

service_ops! {
    op_storage_get = 1,
    op_storage_set = 2,
    op_storage_remove = 3,
    op_storage_clear = 4,
    op_storage_info = 5,
    op_storage_get_async = 6,
    op_storage_set_async = 7,
    op_storage_remove_async = 8,
    op_storage_clear_async = 9,
    op_storage_info_async = 10,
    op_create_buffer_url = 11,
    op_revoke_buffer_url = 12,
}

/// The op's name, for a refusal a person will read.
pub(crate) fn name_of(op: u32) -> Option<&'static str> {
    ALL.iter().find(|(_, id)| *id == op).map(|(name, _)| *name)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The host's table and the contract, entry for entry. A number that
    /// differs is a producer calling one op and the host running another with
    /// the right-looking arguments, which is why this is compared and not
    /// trusted.
    #[test]
    fn the_host_table_is_the_contract() {
        let contract: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../../contracts/runtime/service-ops.json"
        ))
        .expect("the contract is JSON");
        let ops = contract["ops"].as_object().expect("an ops object");
        let mut declared: Vec<(String, u32)> = ops
            .iter()
            .map(|(name, id)| (name.clone(), id.as_u64().expect("a number") as u32))
            .collect();
        declared.sort_by_key(|(_, id)| *id);
        let mut here: Vec<(String, u32)> = ALL
            .iter()
            .map(|(name, id)| (name.to_string(), *id))
            .collect();
        here.sort_by_key(|(_, id)| *id);
        assert_eq!(here, declared);
    }

    #[test]
    fn numbers_are_unique_and_never_zero() {
        let mut seen = std::collections::HashSet::new();
        for (name, id) in ALL {
            assert_ne!(*id, 0, "{name}: zero is not an op number");
            assert!(seen.insert(*id), "{name}: {id} is used twice");
        }
    }
}
