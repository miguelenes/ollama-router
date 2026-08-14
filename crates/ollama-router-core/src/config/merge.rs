//! Deep-merge YAML mappings (overlay wins). Sequences and scalars replace.

use serde_yaml::Value;

/// Merge `overlay` into `base`. Nested mappings are merged recursively;
/// sequences and scalars in `overlay` replace the same key in `base`.
pub(crate) fn deep_merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Mapping(base_map), Value::Mapping(overlay_map)) => {
            for (key, overlay_value) in overlay_map {
                if let Some(existing) = base_map.get_mut(&key) {
                    if existing.is_mapping() && overlay_value.is_mapping() {
                        deep_merge(existing, overlay_value);
                        continue;
                    }
                }
                base_map.insert(key, overlay_value);
            }
        }
        (base, overlay) => *base = overlay,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_maps_merge_and_sequences_replace() {
        let mut base: Value = serde_yaml::from_str(
            "verda:\n  enabled: true\n  auto_scale: true\npolicy:\n  sticky_affinity: false\n",
        )
        .unwrap();
        let overlay: Value =
            serde_yaml::from_str("verda:\n  enabled: false\npolicy:\n  retry_on_status: [503]\n")
                .unwrap();
        deep_merge(&mut base, overlay);
        let merged = base.as_mapping().unwrap();
        let verda = merged.get("verda").unwrap().as_mapping().unwrap();
        assert_eq!(verda.get("enabled").unwrap().as_bool(), Some(false));
        assert_eq!(verda.get("auto_scale").unwrap().as_bool(), Some(true));
        let policy = merged.get("policy").unwrap().as_mapping().unwrap();
        assert_eq!(
            policy
                .get("retry_on_status")
                .unwrap()
                .as_sequence()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            policy.get("sticky_affinity").unwrap().as_bool(),
            Some(false)
        );
    }
}
