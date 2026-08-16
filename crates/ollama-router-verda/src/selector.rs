//! Cheapest available NVIDIA spot selection. Pure; no I/O.

use ollama_router_core::config::{SelectionStrategy, VerdaConfig};

use crate::types::{InstanceAvailability, InstanceType};

#[derive(Clone, Debug, PartialEq)]
pub struct SpotChoice {
    pub instance_type: String,
    pub location_code: String,
    pub spot_price: f64,
    pub gpu_memory_gb: Option<f64>,
    pub gpus: u32,
    pub currency: Option<String>,
}

pub fn glob_match(pat: &str, name: &str) -> bool {
    fn rec(p: &[u8], n: &[u8]) -> bool {
        match (p.first(), n.first()) {
            (None, None) => true,
            (Some(b'*'), _) => rec(&p[1..], n) || (!n.is_empty() && rec(p, &n[1..])),
            (Some(b'?'), Some(_)) => rec(&p[1..], &n[1..]),
            (Some(a), Some(b)) if a.eq_ignore_ascii_case(b) => rec(&p[1..], &n[1..]),
            _ => false,
        }
    }
    rec(pat.as_bytes(), name.as_bytes())
}

fn matches_any(name: &str, globs: &[String]) -> bool {
    globs.is_empty() || globs.iter().any(|g| glob_match(g, name))
}

pub fn availability_pairs(availability: &[InstanceAvailability]) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for entry in availability {
        for instance_type in &entry.availabilities {
            pairs.push((entry.location_code.clone(), instance_type.clone()));
        }
    }
    pairs
}

pub fn rank_candidates(
    availability: &[InstanceAvailability],
    instance_types: &[InstanceType],
    config: &VerdaConfig,
) -> Vec<SpotChoice> {
    let types: std::collections::HashMap<&str, &InstanceType> = instance_types
        .iter()
        .map(|t| (t.instance_type.as_str(), t))
        .collect();
    let preference = &config.allowed_locations;
    let mut candidates = Vec::new();
    for (location_code, instance_type) in availability_pairs(availability) {
        let Some(info) = types.get(instance_type.as_str()) else {
            continue;
        };
        if !info.is_nvidia_gpu() {
            continue;
        }
        let Some(price) = info.spot_price_float() else {
            continue;
        };
        if config
            .max_spot_price_per_hour
            .is_some_and(|max| price > max)
        {
            continue;
        }
        let vram = info.vram_gb();
        match vram {
            Some(v) if config.min_vram_gb > 0.0 && v < config.min_vram_gb => continue,
            Some(v) if config.max_vram_gb.is_some_and(|max| v > max) => continue,
            None if config.min_vram_gb > 0.0 => continue,
            _ => {}
        }
        let gpus = info.gpu_count();
        if gpus < config.min_gpus {
            continue;
        }
        if config.max_gpus.is_some_and(|max| gpus > max) {
            continue;
        }
        if !matches_any(&instance_type, &config.allowed_instance_types) {
            continue;
        }
        if !config.denied_instance_types.is_empty()
            && matches_any(&instance_type, &config.denied_instance_types)
        {
            continue;
        }
        if !config.allowed_locations.is_empty()
            && !config.allowed_locations.iter().any(|l| l == &location_code)
        {
            continue;
        }
        candidates.push(SpotChoice {
            instance_type,
            location_code,
            spot_price: price,
            gpu_memory_gb: vram,
            gpus,
            currency: info.currency.clone(),
        });
    }

    let use_best_value = config.selection_strategy == SelectionStrategy::BestValue;
    candidates.sort_by(|a, b| {
        let pref = |loc: &str| {
            preference
                .iter()
                .position(|p| p == loc)
                .unwrap_or(preference.len())
        };
        let vram = |c: &SpotChoice| c.gpu_memory_gb.unwrap_or(0.0);
        if use_best_value {
            let ratio = |c: &SpotChoice| {
                let v = vram(c);
                if v > 0.0 {
                    (0u8, c.spot_price / v)
                } else {
                    (1u8, f64::INFINITY)
                }
            };
            ratio(a)
                .0
                .cmp(&ratio(b).0)
                .then(
                    ratio(a)
                        .1
                        .partial_cmp(&ratio(b).1)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(
                    a.spot_price
                        .partial_cmp(&b.spot_price)
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(
                    vram(a)
                        .partial_cmp(&vram(b))
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(a.gpus.cmp(&b.gpus))
                .then(pref(&a.location_code).cmp(&pref(&b.location_code)))
                .then(a.instance_type.cmp(&b.instance_type))
        } else {
            a.spot_price
                .partial_cmp(&b.spot_price)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(
                    vram(a)
                        .partial_cmp(&vram(b))
                        .unwrap_or(std::cmp::Ordering::Equal),
                )
                .then(a.gpus.cmp(&b.gpus))
                .then(pref(&a.location_code).cmp(&pref(&b.location_code)))
                .then(a.instance_type.cmp(&b.instance_type))
        }
    });
    candidates
}

pub fn pick_cheapest_available_spot_gpu(
    availability: &[InstanceAvailability],
    instance_types: &[InstanceType],
    config: &VerdaConfig,
) -> Option<SpotChoice> {
    rank_candidates(availability, instance_types, config)
        .into_iter()
        .next()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{GpuMemorySpec, GpuSpec};
    use ollama_router_core::config::SelectionStrategy;

    fn nvidia(ty: &str, price: &str, vram: f64, gpus: u32) -> InstanceType {
        InstanceType {
            instance_type: ty.into(),
            manufacturer: Some("NVIDIA".into()),
            spot_price: Some(price.into()),
            gpu: Some(GpuSpec {
                number_of_gpus: Some(gpus),
                manufacturer: Some("NVIDIA".into()),
                ..GpuSpec::default()
            }),
            gpu_memory: Some(GpuMemorySpec {
                size_in_gigabytes: Some(vram),
            }),
            ..InstanceType::default()
        }
    }

    fn avail(loc: &str, types: &[&str]) -> InstanceAvailability {
        InstanceAvailability {
            location_code: loc.into(),
            availabilities: types.iter().map(|s| (*s).to_string()).collect(),
        }
    }

    fn cheapest_config() -> VerdaConfig {
        VerdaConfig {
            selection_strategy: SelectionStrategy::Cheapest,
            ..VerdaConfig::default()
        }
    }

    #[test]
    fn cheapest_nvidia_smallest_vram_on_tie() {
        let config = cheapest_config();
        let types = [
            nvidia("big", "0.40", 80.0, 1),
            nvidia("small", "0.40", 24.0, 1),
            nvidia("cpu", "0.10", 0.0, 0),
        ];
        let mut cpu = types[2].clone();
        cpu.manufacturer = Some("AMD".into());
        cpu.gpu = None;
        cpu.gpu_memory = None;
        cpu.spot_price = Some("0.10".into());
        let types = vec![types[0].clone(), types[1].clone(), cpu];
        let availability = vec![avail("HEL", &["big", "small", "cpu"])];
        let pick = pick_cheapest_available_spot_gpu(&availability, &types, &config).unwrap();
        assert_eq!(pick.instance_type, "small");
    }

    #[test]
    fn never_ranks_on_demand_without_spot() {
        let config = VerdaConfig::default();
        let mut t = nvidia("ondemand", "1.00", 24.0, 1);
        t.spot_price = None;
        t.price_per_hour = Some("1.00".into());
        let availability = vec![avail("HEL", &["ondemand"])];
        assert!(pick_cheapest_available_spot_gpu(&availability, &[t], &config).is_none());
    }

    #[test]
    fn vram_window_filters() {
        let config = VerdaConfig {
            min_vram_gb: 8.0,
            max_vram_gb: Some(80.0),
            ..VerdaConfig::default()
        };
        let types = [
            nvidia("tiny", "0.10", 4.0, 1),
            nvidia("ok", "0.50", 24.0, 1),
            nvidia("huge", "0.90", 141.0, 1),
        ];
        let availability = vec![avail("HEL", &["tiny", "ok", "huge"])];
        let ranked = rank_candidates(&availability, &types, &config);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].instance_type, "ok");
    }

    #[test]
    fn cheapest_larger_gpu_beats_expensive_smaller() {
        let config = cheapest_config();
        let types = [
            nvidia("small", "0.50", 24.0, 1),
            nvidia("large", "0.20", 80.0, 1),
        ];
        let availability = vec![avail("HEL", &["small", "large"])];
        let pick = pick_cheapest_available_spot_gpu(&availability, &types, &config).unwrap();
        assert_eq!(pick.instance_type, "large");
    }

    #[test]
    fn best_value_ranks_by_price_per_vram() {
        // Default is BestValue; cheapest_config must set Cheapest explicitly.
        let config = VerdaConfig::default();
        assert_eq!(config.selection_strategy, SelectionStrategy::BestValue);
        let types = [
            nvidia("small", "0.40", 24.0, 1),
            nvidia("large", "0.80", 80.0, 1),
        ];
        let availability = vec![avail("HEL", &["small", "large"])];
        let pick = pick_cheapest_available_spot_gpu(&availability, &types, &config).unwrap();
        assert_eq!(pick.instance_type, "large");
        let cheap =
            pick_cheapest_available_spot_gpu(&availability, &types, &cheapest_config()).unwrap();
        assert_eq!(cheap.instance_type, "small");
    }

    #[test]
    fn better_value_wins_over_cheaper_sticker() {
        // 0.40/48 ≈ 0.0083 $/GiB beats 0.20/8 = 0.025 $/GiB.
        let config = VerdaConfig::default();
        let types = [
            nvidia("tiny_cheap", "0.20", 8.0, 1),
            nvidia("mid_value", "0.40", 48.0, 1),
        ];
        let availability = vec![avail("HEL", &["tiny_cheap", "mid_value"])];
        let pick = pick_cheapest_available_spot_gpu(&availability, &types, &config).unwrap();
        assert_eq!(pick.instance_type, "mid_value");
    }

    #[test]
    fn over_cap_offer_is_rejected() {
        let config = VerdaConfig {
            max_spot_price_per_hour: Some(0.30),
            ..VerdaConfig::default()
        };
        let types = [
            nvidia("cheap", "0.20", 8.0, 1),
            nvidia("over", "0.40", 48.0, 1),
        ];
        let availability = vec![avail("HEL", &["cheap", "over"])];
        let ranked = rank_candidates(&availability, &types, &config);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].instance_type, "cheap");
    }

    #[test]
    fn best_value_unknown_vram_sorts_last() {
        let config = VerdaConfig {
            min_vram_gb: 0.0,
            max_vram_gb: None,
            ..VerdaConfig::default()
        };
        let mut unknown = nvidia("mystery", "0.05", 24.0, 1);
        unknown.gpu_memory = None;
        let types = [nvidia("known", "0.40", 24.0, 1), unknown];
        let availability = vec![avail("HEL", &["known", "mystery"])];
        let ranked = rank_candidates(&availability, &types, &config);
        assert_eq!(ranked[0].instance_type, "known");
        assert_eq!(ranked[1].instance_type, "mystery");
    }

    #[test]
    fn explicit_cheapest_ranks_by_hourly_price() {
        let config = cheapest_config();
        let types = [
            nvidia("pricey_value", "0.40", 48.0, 1),
            nvidia("cheap_sticker", "0.20", 8.0, 1),
        ];
        let availability = vec![avail("HEL", &["pricey_value", "cheap_sticker"])];
        let pick = pick_cheapest_available_spot_gpu(&availability, &types, &config).unwrap();
        assert_eq!(pick.instance_type, "cheap_sticker");
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use crate::types::{GpuMemorySpec, GpuSpec};
    use ollama_router_core::config::SelectionStrategy;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn cheapest_is_min_spot_price(
            prices in proptest::collection::vec(0.01f64..5.0, 1..8)
        ) {
            let types: Vec<InstanceType> = prices.iter().enumerate().map(|(i, p)| InstanceType {
                instance_type: format!("t{i}"),
                manufacturer: Some("NVIDIA".into()),
                spot_price: Some(format!("{p:.4}")),
                gpu: Some(GpuSpec { number_of_gpus: Some(1), manufacturer: Some("NVIDIA".into()), ..GpuSpec::default() }),
                gpu_memory: Some(GpuMemorySpec { size_in_gigabytes: Some(24.0) }),
                ..InstanceType::default()
            }).collect();
            let names: Vec<String> = types.iter().map(|t| t.instance_type.clone()).collect();
            let availability = vec![InstanceAvailability {
                location_code: "HEL".into(),
                availabilities: names,
            }];
            let config = VerdaConfig {
                selection_strategy: SelectionStrategy::Cheapest,
                ..VerdaConfig::default()
            };
            let pick = pick_cheapest_available_spot_gpu(&availability, &types, &config).unwrap();
            let min = types
                .iter()
                .filter_map(|t| t.spot_price_float())
                .fold(f64::INFINITY, f64::min);
            prop_assert!((pick.spot_price - min).abs() < 1e-6);
        }
    }
}
