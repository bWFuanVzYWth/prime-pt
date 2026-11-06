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

/// 已发布候选灯的能力上界，与资源发布时的Flow/side过滤相同。
/// 输入等长由prepare_material_generation建立；不把接收材质的MERS误当作发光纹理。
pub(super) const fn emitter_capabilities(
    definitions: &[MaterialDefinition],
    flows: &[openpbr::FlowKey],
) -> u32 {
    let mut capabilities = 0;
    let mut index = 0;
    while index < definitions.len() {
        let definition = &definitions[index];
        if flows[index].emits() && emission_sides(definition) != 0 {
            if definition.control[3] >> 1 != 0 {
                capabilities |= 1;
            }
            if definition.control[2] & MaterialDefinition::CUTOUT != 0 {
                capabilities |= 2;
            }
            if definition.control[2] & MaterialDefinition::TEXTURED_EMISSION != 0 {
                capabilities |= 4;
            }
        }
        index += 1;
    }
    capabilities
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
        | if flow.emits() && material.control[3] >> 1 != 0 {
            1 << 10
        } else {
            0
        }
        | ((material.control[0] & 3) << 6)
        | ((flags & (MaterialDefinition::OVERLAY | MaterialDefinition::OVERLAY_MERS)) << 1)
}

/// 重放/求值不消费发光，不能让发光面、朝向和贴图位扩大它们的程序键。
pub(super) const fn scattering_key(
    (flow, variant): (openpbr::FlowKey, u32),
) -> (openpbr::FlowKey, u32) {
    (
        flow.without_emission(),
        variant & !(4 | 16 | 32 | (1 << 10)),
    )
}

/// 初采消费完整散射与发光；命中灯逆向提案由独立alias能力决定。
pub(super) const fn initial_key(key: (openpbr::FlowKey, u32)) -> (openpbr::FlowKey, u32) {
    key
}

/// 初采不延续时仅连续方向与发光有消费者；纯delta降为发光专用或零程序。
pub(super) const fn initial_direct_key(key: (openpbr::FlowKey, u32)) -> (openpbr::FlowKey, u32) {
    if key.0.continuous() {
        initial_key(key)
    } else {
        emitter_key(key)
    }
}

const fn operation_flow(bits: u32) -> openpbr::FlowKey {
    match openpbr::FlowKey::from_bits(bits) {
        Ok(flow) => flow,
        Err(_) => panic!("内部操作能力组合无效"),
    }
}

/// 表面元数据不消费散射颜色、发光或命中光源的逆向提案密度。
pub(super) const fn metadata_key(
    (flow, variant): (openpbr::FlowKey, u32),
) -> (openpbr::FlowKey, u32) {
    let variant = variant & !(4 | 16 | 32 | (1 << 10));
    let variant = if variant & 3 == 0 {
        variant & !((1 << 8) | (1 << 9))
    } else {
        variant
    };
    let classification = if flow.bits()
        & (openpbr::FlowKey::DELTA_REFLECTION | openpbr::FlowKey::DELTA_TRANSMISSION)
        == 0
        || flow.bits()
            & (openpbr::FlowKey::SOLID_TRANSMISSION
                | openpbr::FlowKey::THIN_TRANSMISSION
                | openpbr::FlowKey::CONDUCTOR_REFLECTION)
            == 0
    {
        operation_flow(0)
    } else {
        flow.without_emission()
    };
    (classification, variant)
}

/// 无连续lobe时纯求值恒为零，不按无消费者的贴图/法线能力复制入口。
pub(super) const fn evaluate_key(key: (openpbr::FlowKey, u32)) -> (openpbr::FlowKey, u32) {
    if key.0.continuous() {
        scattering_key(key)
    } else {
        (operation_flow(0), 0)
    }
}

/// 连接Beer仅solid需要几何穿界；离散表面的连续BSDF求值本身恒为零。
pub(super) const fn connection_key(key: (openpbr::FlowKey, u32)) -> (openpbr::FlowKey, u32) {
    if key.0.continuous() {
        scattering_key(key)
    } else if key.0.bits() & openpbr::FlowKey::SOLID_TRANSMISSION != 0 {
        (
            operation_flow(
                openpbr::FlowKey::SOLID_TRANSMISSION | openpbr::FlowKey::DELTA_TRANSMISSION,
            ),
            64,
        )
    } else {
        (operation_flow(0), 0)
    }
}

/// 已命中发光端只读发光侧、发光贴图和alias；overlay仅影响旧纹理发光的UV。
pub(super) const fn emitter_key(
    (flow, variant): (openpbr::FlowKey, u32),
) -> (openpbr::FlowKey, u32) {
    if !flow.emits() {
        return (operation_flow(0), 0);
    }
    let overlay = if variant & 4 != 0 {
        variant & (1 << 8)
    } else {
        0
    };
    (
        operation_flow(openpbr::FlowKey::EMISSION),
        variant & (4 | 16 | 32 | (1 << 10)) | overlay,
    )
}

/// 逆向提案只消费命中灯alias形态；不消费材料RGB、MERS、IOR或法线。
pub(super) const fn pdf_key((_flow, variant): (openpbr::FlowKey, u32)) -> (openpbr::FlowKey, u32) {
    (operation_flow(0), variant & (1 << 10))
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

    #[test]
    fn alias_presence_only_separates_operations_that_read_the_emitter_proposal() {
        let flow = simple_engine::openpbr::Material::opaque([0.5; 3], 0.4, 1.5, 0., 0.)
            .unwrap()
            .flow(true);
        let mut source = MaterialDefinition {
            emission: [1.; 4],
            ..Default::default()
        };
        let plain = (flow, variant(flow, &source));
        source.control[3] = 2;
        let aliased = (flow, variant(flow, &source));
        assert_ne!(initial_key(plain), initial_key(aliased));
        assert_ne!(emitter_key(plain), emitter_key(aliased));
        assert_ne!(pdf_key(plain), pdf_key(aliased));
        assert_eq!(scattering_key(plain), scattering_key(aliased));
        assert_eq!(metadata_key(plain), metadata_key(aliased));
    }

    #[test]
    fn delta_evaluation_is_shared_but_solid_connection_keeps_medium_crossing() {
        let glass = simple_engine::openpbr::Material::dielectric(0., None)
            .unwrap()
            .flow(false);
        let metal = simple_engine::openpbr::Material::opaque([0.5; 3], 0., 1.5, 1., 0.)
            .unwrap()
            .flow(false);
        assert_eq!(
            evaluate_key((glass, 64 | 1 | 2 | 8)),
            evaluate_key((metal, 1))
        );
        assert_ne!(
            connection_key((glass, 64 | 1 | 2 | 8)),
            connection_key((metal, 1))
        );
    }

    #[test]
    fn metadata_does_not_split_on_scattering_lobes_or_emission() {
        let diffuse = simple_engine::openpbr::Material::opaque([0.5; 3], 0.4, 1.5, 0., 0.)
            .unwrap()
            .flow(false);
        let sss = simple_engine::openpbr::Material::opaque([0.5; 3], 0.4, 1.5, 0., 1.)
            .unwrap()
            .flow(true);
        assert_eq!(
            metadata_key((diffuse, 1 | 2 | 8)),
            metadata_key((sss, 1 | 2 | 8 | 4 | 16 | 32 | 1024))
        );
    }

    #[test]
    fn terminal_initial_shares_delta_surfaces_but_keeps_continuous_proposals() {
        let glass = simple_engine::openpbr::Material::dielectric(0., None)
            .unwrap()
            .flow(false);
        let metal = simple_engine::openpbr::Material::opaque([0.5; 3], 0., 1.5, 1., 0.)
            .unwrap()
            .flow(false);
        assert_eq!(
            initial_direct_key((glass, 64 | 1 | 2 | 8)),
            initial_direct_key((metal, 1))
        );
        let diffuse = simple_engine::openpbr::Material::opaque([0.5; 3], 0.4, 1.5, 0., 0.)
            .unwrap()
            .flow(false);
        assert_eq!(
            initial_direct_key((diffuse, 1 | 2 | 8)),
            (diffuse, 1 | 2 | 8)
        );
    }

    #[test]
    fn selected_emitter_bounds_follow_the_published_light_domain() {
        let plain = MaterialDefinition {
            emission: [1.; 4],
            control: [0, 1, 0, 0],
            ..Default::default()
        };
        let mut mapped = plain;
        mapped.control[2] = MaterialDefinition::MERS
            | MaterialDefinition::TEXTURED_EMISSION
            | MaterialDefinition::CUTOUT;
        mapped.control[3] = 2;
        let definitions = [plain, mapped];
        let colors = colors(&[255; 4]).unwrap();
        for (emission, expected) in [(0, 0), (255, 7)] {
            let flows = flows(
                colors,
                &definitions,
                map_ranges(&[0, emission, 255, 0], &[]).unwrap(),
            )
            .unwrap();
            assert!(flows[0].emits());
            assert_eq!(flows[1].emits(), emission != 0);
            assert_eq!(emitter_capabilities(&definitions, &flows), expected);
        }
    }
}
