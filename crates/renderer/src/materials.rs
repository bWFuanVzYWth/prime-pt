//! 材质世代的真实资源范围分析；同一atlas只扫描一次，不按材质重复扫描整图。
use crate::gpu::Error;
use prime_scene::MaterialDefinition;
use simple_engine::{openpbr, scene_cache};

/// 只有这些覆盖规则会拒绝硬件三角候选；透明度和光学透射彼此独立。
pub(super) const fn tests_coverage(definition: &MaterialDefinition) -> bool {
    coverage_flags(definition) != 0
}

const fn coverage_flags(definition: &MaterialDefinition) -> u32 {
    definition.control[2] & (MaterialDefinition::CUTOUT | MaterialDefinition::FRONT_ONLY)
}

/// 资源世代内命中阶段可能用到的材质能力。
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) struct HitFeatures {
    pub coverage: u32,
    pub authored_normals: bool,
}

pub(super) fn hit_features(definitions: &[MaterialDefinition]) -> HitFeatures {
    let mut features = HitFeatures {
        coverage: 0,
        authored_normals: false,
    };
    for definition in definitions {
        features.coverage |= coverage_flags(definition);
        features.authored_normals |=
            definition.control[3] & MaterialDefinition::AUTHORED_NORMAL != 0;
    }
    features
}

pub(super) const fn emission_sides(material: &MaterialDefinition) -> u32 {
    if material.control[2] & MaterialDefinition::FRONT_ONLY != 0 {
        material.control[1] & 1
    } else {
        material.control[1]
    }
}

/// 资源期的静态亮度提案；保留动态tint和MERS/动画实际求值的宿主适配边界。
/// 与Game共用线性亮度×幅度公式，source RGB由本适配器解释，不扫描运行期几何。
pub(super) fn emission_weights(definitions: &[MaterialDefinition]) -> Result<Vec<f32>, Error> {
    let mut weights = Vec::new();
    weights
        .try_reserve_exact(definitions.len())
        .map_err(scene_cache::Error::from)?;
    for definition in definitions {
        let [r, g, b, amplitude] = definition.emission;
        weights.push(simple_engine::light_sampling::emission_weight(
            [r, g, b],
            amplitude,
        )?);
    }
    Ok(weights)
}

/// `mod_materials.slang` 的场景材质能力位。只影响分支，相同路径仍共享数值参数表。
pub(super) const fn variant(flow: openpbr::FlowKey, material: &MaterialDefinition) -> u32 {
    let flags = material.control[2];
    let maps = flags & (MaterialDefinition::MERS | MaterialDefinition::AUXILIARY);
    let normal = if (maps & MaterialDefinition::AUXILIARY) != 0 {
        (material.control[3] & MaterialDefinition::AUTHORED_NORMAL) << 3
    } else {
        0
    };
    let emission = if flow.bits() & openpbr::FlowKey::EMISSION != 0 {
        let sides = emission_sides(material);
        if sides == 0 {
            0
        } else {
            (sides << 4) | (flags & MaterialDefinition::TEXTURED_EMISSION)
        }
    } else {
        0
    };
    maps | normal
        | emission
        | ((material.control[0] & 3) << 6)
        | ((flags & (MaterialDefinition::OVERLAY | MaterialDefinition::OVERLAY_MERS)) << 1)
}

/// 重放/求值不消费发光，不能让发光面、朝向和贴图位扩大它们的程序键。
pub(super) const fn scattering_key(
    (flow, variant): (openpbr::FlowKey, u32),
) -> (openpbr::FlowKey, u32) {
    (flow.without_emission(), variant & !(4 | 16 | 32))
}

pub(super) fn defaults() -> Vec<MaterialDefinition> {
    let rough = MaterialDefinition::default();
    let mut smooth = rough;
    smooth.base_roughness[3] = 0.25;
    vec![rough, smooth]
}

pub(super) fn colors(pixels: &[u8]) -> Result<[openpbr::resource::ColorRanges; 2], Error> {
    use simple_engine::openpbr::resource::{ColorScan, Coverage, Filtering, Texels};
    let unit = openpbr::Range::new(0., 1.)?;
    let scan = |coverage| -> Result<_, Error> {
        let mut colors = ColorScan::new(coverage, Filtering::Nearest)?;
        colors.extend(Texels::Srgb8Rgba(pixels))?;
        Ok(colors.finish([unit; 3])?)
    };
    Ok([scan(Coverage::Opaque)?, scan(Coverage::AlphaAtLeast(0.5))?])
}

#[derive(Clone, Copy)]
pub(super) struct MapRanges {
    roughness: openpbr::Range,
    metalness: openpbr::Range,
    subsurface: openpbr::Range,
    emission: openpbr::Range,
    normal_distribution: openpbr::Range,
}

pub(super) fn map_ranges(mers: &[u8], auxiliary: &[u8]) -> Result<MapRanges, Error> {
    let unit = openpbr::Range::new(0., 1.)?;
    let mut scans = [openpbr::RangeScan::new(); 4];
    for pixel in mers.as_chunks::<4>().0 {
        for (scan, value) in scans.iter_mut().zip(pixel) {
            scan.extend(&[f32::from(*value) / 255.])?;
        }
    }
    let [metalness, emission, roughness, subsurface] =
        scans.map(|scan| scan.finish().unwrap_or(unit));
    let mut distribution = openpbr::RangeScan::new();
    for pixel in auxiliary.as_chunks::<4>().0 {
        distribution.extend(&[f32::from(pixel[2]) / 255.])?;
    }
    Ok(MapRanges {
        roughness,
        metalness,
        subsurface,
        emission,
        normal_distribution: distribution.finish().unwrap_or(unit),
    })
}

pub(super) fn flows(
    colors: [openpbr::resource::ColorRanges; 2],
    definitions: &[MaterialDefinition],
    textured: MapRanges,
) -> Result<Vec<openpbr::FlowKey>, Error> {
    use simple_engine::openpbr::resource::{ColorRanges, MaterialBinding};
    use simple_engine::openpbr::{InterfaceIor, MaterialRanges, Range};
    if definitions.is_empty() || definitions.iter().any(|m| !m.valid()) {
        return Err(Error::Invalid("材质表为空或字段不符合OpenPBR输入域"));
    }
    let mut flows = Vec::new();
    flows
        .try_reserve_exact(definitions.len())
        .map_err(scene_cache::Error::from)?;
    for definition in definitions {
        let flags = definition.control[2];
        let base_mers = flags & MaterialDefinition::MERS != 0;
        let top_mers = flags & MaterialDefinition::OVERLAY_MERS != 0;
        let layer_mismatch = flags & MaterialDefinition::OVERLAY != 0 && base_mers != top_mers;
        let property_range = |value, texture: Range| {
            if layer_mismatch {
                Ok(texture.union(Range::point(value)?))
            } else if base_mers {
                Ok(texture)
            } else {
                Range::point(value)
            }
        };
        let roughness = property_range(definition.base_roughness[3], textured.roughness)?;
        let roughness = if definition.control[3] & MaterialDefinition::AUTHORED_NORMAL != 0
            && definition.control[2] & MaterialDefinition::AUXILIARY != 0
        {
            roughness.with_normal_distribution(textured.normal_distribution)?
        } else {
            roughness
        };
        let ranges = if definition.control[0] & 1 != 0 {
            MaterialRanges::dielectric(roughness, definition.control[0] & 2 != 0)?
        } else {
            MaterialRanges::opaque(
                roughness,
                property_range(definition.surface[1], textured.metalness)?,
                property_range(definition.surface[2], textured.subsurface)?,
            )?
        };
        let colors = colors[usize::from(definition.control[2] & MaterialDefinition::CUTOUT != 0)];
        let reaches_rgb = |factor: &[f32]| {
            factor
                .iter()
                .zip(colors.rgb())
                .any(|(&value, range)| value > 0. && range.max() > 0.)
        };
        let emission = emission_sides(definition) != 0
            && definition.emission[3] > 0.
            && reaches_rgb(&definition.emission[..3])
            && (definition.control[2] & MaterialDefinition::TEXTURED_EMISSION == 0
                || textured.emission.max() > 0.);
        // 只证明零域，正值保留整个原范围，避免在此处重复着色器舍入规则。
        let base = if reaches_rgb(&definition.base_roughness[..3]) {
            colors
        } else {
            ColorRanges::constant([0.; 3])?
        };
        flows.push(MaterialBinding::new(ranges, base, InterfaceIor::UNKNOWN, emission)?.flow());
    }
    Ok(flows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn black_mers_emission_excludes_the_light_path() {
        let mut material = MaterialDefinition {
            emission: [1.; 4],
            ..Default::default()
        };
        material.control[2] |= MaterialDefinition::TEXTURED_EMISSION;
        let colors = colors(&[255; 4]).unwrap();
        let flow = |emission| {
            flows(
                colors,
                &[material],
                map_ranges(&[0, emission, 255, 0], &[]).unwrap(),
            )
            .unwrap()[0]
        };
        assert!(!flow(0).emits());
        assert!(flow(255).emits());
    }

    #[test]
    fn normal_distribution_keeps_continuous_reflection_in_the_program_support() {
        let mut material = MaterialDefinition::default();
        material.base_roughness[3] = 0.;
        material.control[3] = MaterialDefinition::AUTHORED_NORMAL;
        material.control[2] |= MaterialDefinition::AUXILIARY;
        let flow = flows(
            colors(&[255; 4]).unwrap(),
            &[material],
            map_ranges(&[], &[]).unwrap(),
        )
        .unwrap();
        assert_ne!(flow[0].bits() & openpbr::FlowKey::CONTINUOUS_REFLECTION, 0);
    }

    #[test]
    fn rough_authored_normal_material_excludes_delta_reflection() {
        let mut material = MaterialDefinition::default();
        material.control[3] = MaterialDefinition::AUTHORED_NORMAL;
        material.control[2] |= MaterialDefinition::AUXILIARY;
        let flow = flows(
            colors(&[255; 4]).unwrap(),
            &[material],
            map_ranges(&[], &[128, 128, 0, 85]).unwrap(),
        )
        .unwrap()[0];
        assert_eq!(flow.bits() & openpbr::FlowKey::DELTA_REFLECTION, 0);
    }

    #[test]
    fn smooth_normal_distribution_excludes_continuous_reflection() {
        let mut material = MaterialDefinition::default();
        material.base_roughness[3] = 0.;
        material.control[3] = MaterialDefinition::AUTHORED_NORMAL;
        material.control[2] |= MaterialDefinition::AUXILIARY;
        let flow = flows(
            colors(&[255; 4]).unwrap(),
            &[material],
            map_ranges(&[], &[128, 128, 0, 85]).unwrap(),
        )
        .unwrap()[0];
        assert_eq!(flow.bits() & openpbr::FlowKey::CONTINUOUS_REFLECTION, 0);
    }

    #[test]
    fn scattering_program_ignores_emission_but_keeps_material_maps() {
        let flow = simple_engine::openpbr::Material::opaque([0.5; 3], 0.4, 1.5, 0., 0.)
            .unwrap()
            .flow(true);
        let (key, variant) = scattering_key((flow, 1 | 2 | 4 | 8 | 16 | 32));
        assert!(!key.emits());
        assert_eq!(variant, 1 | 2 | 8);
    }
}
