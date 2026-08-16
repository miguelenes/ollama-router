//! Best price-per-VRAM GPU selection. Pure; no I/O.

use ollama_router_core::config::RunpodConfig;

use crate::types::CatalogGpu;

#[derive(Clone, Debug, PartialEq)]
pub struct GpuChoice {
    pub gpu_type_id: String,
    pub vram_gb: f64,
    pub on_demand_price: f64,
    pub data_center: Option<String>,
}

fn glob_match(pat: &str, name: &str) -> bool {
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

fn data_center_ok(gpu: &CatalogGpu, allowed: &[String]) -> Option<Option<String>> {
    if allowed.is_empty() {
        return Some(None);
    }
    if gpu.data_centers.is_empty() {
        // No per-DC expansion; accept if overall availability already passed.
        return Some(None);
    }
    for dc in &gpu.data_centers {
        let Some(id) = dc.id.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
            continue;
        };
        if !allowed.iter().any(|a| a == id) {
            continue;
        }
        let avail = dc.availability.as_deref().unwrap_or("HIGH");
        if avail.eq_ignore_ascii_case("NONE") {
            continue;
        }
        return Some(Some(id.to_string()));
    }
    None
}

/// Filter catalog rows and rank ascending by on-demand price per VRAM GiB.
pub fn rank_gpu_types(gpus: &[CatalogGpu], config: &RunpodConfig) -> Vec<GpuChoice> {
    let mut candidates = Vec::new();
    for gpu in gpus {
        if !gpu.is_available() {
            continue;
        }
        let Some(id) = gpu.gpu_type_id().map(str::to_string) else {
            continue;
        };
        if !matches_any(&id, &config.allowed_gpu_types) {
            continue;
        }
        if !config.denied_gpu_types.is_empty() && matches_any(&id, &config.denied_gpu_types) {
            continue;
        }
        let Some(vram) = gpu.memory.filter(|v| *v > 0.0) else {
            continue;
        };
        if config.min_vram_gb > 0.0 && vram < config.min_vram_gb {
            continue;
        }
        if config.max_vram_gb.is_some_and(|max| vram > max) {
            continue;
        }
        let Some(price) = gpu.on_demand_price(&config.cloud_type) else {
            continue;
        };
        if config.max_price_per_hour.is_some_and(|max| price > max) {
            continue;
        }
        let Some(dc) = data_center_ok(gpu, &config.allowed_data_centers) else {
            continue;
        };
        candidates.push(GpuChoice {
            gpu_type_id: id,
            vram_gb: vram,
            on_demand_price: price,
            data_center: dc,
        });
    }
    candidates.sort_by(|a, b| {
        let ratio = |c: &GpuChoice| c.on_demand_price / c.vram_gb;
        ratio(a)
            .partial_cmp(&ratio(b))
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.on_demand_price
                    .partial_cmp(&b.on_demand_price)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(
                a.vram_gb
                    .partial_cmp(&b.vram_gb)
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
            .then(a.gpu_type_id.cmp(&b.gpu_type_id))
    });
    candidates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CatalogPrice;

    fn gpu(id: &str, vram: f64, price: f64, avail: &str) -> CatalogGpu {
        CatalogGpu {
            id: Some(id.into()),
            name: Some(id.into()),
            memory: Some(vram),
            price: Some(CatalogPrice {
                secure: Some(price),
                community: Some(price),
            }),
            availability: Some(avail.into()),
            ..CatalogGpu::default()
        }
    }

    #[test]
    fn cheapest_per_gib_eligible_gpu_wins() {
        let config = RunpodConfig::default();
        // 0.40/48 ≈ 0.0083 beats 0.20/8 = 0.025
        let gpus = [
            gpu("tiny_cheap", 8.0, 0.20, "HIGH"),
            gpu("mid_value", 48.0, 0.40, "HIGH"),
        ];
        let ranked = rank_gpu_types(&gpus, &config);
        assert_eq!(ranked[0].gpu_type_id, "mid_value");
        assert_eq!(ranked[1].gpu_type_id, "tiny_cheap");
    }

    #[test]
    fn over_cap_gpu_is_skipped_even_if_it_would_win_on_value() {
        let config = RunpodConfig {
            max_price_per_hour: Some(0.30),
            ..RunpodConfig::default()
        };
        let gpus = [
            gpu("over", 80.0, 0.40, "HIGH"),
            gpu("ok", 24.0, 0.25, "HIGH"),
        ];
        let ranked = rank_gpu_types(&gpus, &config);
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].gpu_type_id, "ok");
    }

    #[test]
    fn nothing_eligible_means_empty() {
        let config = RunpodConfig {
            min_vram_gb: 24.0,
            max_vram_gb: Some(24.0),
            max_price_per_hour: Some(0.10),
            ..RunpodConfig::default()
        };
        let gpus = [
            gpu("tiny", 8.0, 0.05, "HIGH"),
            gpu("pricey", 24.0, 0.50, "HIGH"),
            gpu("gone", 24.0, 0.05, "NONE"),
        ];
        assert!(rank_gpu_types(&gpus, &config).is_empty());
    }
}

#[cfg(test)]
mod proptests {
    use super::*;
    use crate::types::CatalogPrice;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn cap_and_band_filter_dominate_ranking(
            prices in proptest::collection::vec(0.01f64..5.0, 2..8),
            cap in 0.05f64..2.0,
            min_vram in 4.0f64..20.0,
        ) {
            let max_vram = min_vram + 40.0;
            let gpus: Vec<CatalogGpu> = prices.iter().enumerate().map(|(i, p)| {
                let vram = if i % 2 == 0 { min_vram - 1.0 } else { (min_vram + max_vram) / 2.0 };
                CatalogGpu {
                    id: Some(format!("g{i}")),
                    memory: Some(vram),
                    price: Some(CatalogPrice { secure: Some(*p), community: Some(*p) }),
                    availability: Some("HIGH".into()),
                    ..CatalogGpu::default()
                }
            }).collect();
            let config = RunpodConfig {
                min_vram_gb: min_vram,
                max_vram_gb: Some(max_vram),
                max_price_per_hour: Some(cap),
                ..RunpodConfig::default()
            };
            let ranked = rank_gpu_types(&gpus, &config);
            for choice in &ranked {
                prop_assert!(choice.vram_gb >= min_vram - 1e-9);
                prop_assert!(choice.vram_gb <= max_vram + 1e-9);
                prop_assert!(choice.on_demand_price <= cap + 1e-9);
            }
            // Any GPU outside the band/cap must not appear, even if cheaper per GiB.
            for gpu in &gpus {
                let id = gpu.gpu_type_id().unwrap();
                let vram = gpu.memory.unwrap();
                let price = gpu.on_demand_price("SECURE").unwrap();
                let eligible = vram >= min_vram && vram <= max_vram && price <= cap;
                let present = ranked.iter().any(|c| c.gpu_type_id == id);
                prop_assert_eq!(eligible, present);
            }
        }
    }
}
