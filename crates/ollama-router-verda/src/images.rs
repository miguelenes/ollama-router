//! Ubuntu 24.04 image selection for Verda spots.

use ollama_router_core::config::VerdaConfig;

use crate::selector::glob_match;
use crate::types::{Image, InstanceType};

pub fn pick_ubuntu24_nvidia_docker_image(
    images: &[Image],
    config: &VerdaConfig,
    instance_type: Option<&InstanceType>,
) -> Option<String> {
    if images.is_empty() {
        return None;
    }
    let supported: std::collections::HashSet<String> = instance_type
        .map(|t| {
            t.supported_os
                .iter()
                .map(|s| s.trim().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let mut names: Vec<&str> = images.iter().map(|i| i.image_type.as_str()).collect();
    if !supported.is_empty() {
        names.retain(|n| supported.contains(&n.to_ascii_lowercase()));
    }
    let globs = if config.preferred_image_globs.is_empty() {
        vec![
            "*ubuntu-24*cuda*docker*".to_string(),
            "*ubuntu-24*docker*".to_string(),
            "ubuntu-24.04".to_string(),
        ]
    } else {
        config.preferred_image_globs.clone()
    };
    for pattern in &globs {
        for name in &names {
            if glob_match(pattern, name) {
                return Some((*name).to_string());
            }
        }
    }
    names
        .into_iter()
        .find(|n| {
            let l = n.to_ascii_lowercase();
            l.contains("ubuntu-24") || l.contains("ubuntu24")
        })
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn img(t: &str) -> Image {
        Image {
            image_type: t.into(),
            ..Image::default()
        }
    }

    #[test]
    fn prefers_cuda_docker() {
        let images = vec![
            img("ubuntu-24.04"),
            img("ubuntu-24-docker"),
            img("ubuntu-24-cuda-docker"),
        ];
        let config = VerdaConfig::default();
        assert_eq!(
            pick_ubuntu24_nvidia_docker_image(&images, &config, None).as_deref(),
            Some("ubuntu-24-cuda-docker")
        );
    }
}
