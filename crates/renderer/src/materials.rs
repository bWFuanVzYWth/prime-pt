//! 材质世代的真实资源范围分析；同一atlas只扫描一次，不按材质重复扫描整图。
use crate::gpu::Error;
use prime_scene::MaterialDefinition;

pub(super) fn defaults() -> Vec<MaterialDefinition> {
    let rough = MaterialDefinition::default();
    let mut smooth = rough;
    smooth.base_roughness[3] = 0.25;
    vec![rough, smooth]
}

pub(super) fn colors(pixels: &[u8]) -> Result<[openpbr::resource::ColorRanges; 2], Error> {
    use openpbr::resource::{ColorScan, Coverage, Filtering, Texels};
    let unit = openpbr::Range::new(0., 1.)?;
    let scan = |coverage| -> Result<_, Error> {
        let mut colors = ColorScan::new(coverage, Filtering::Nearest)?;
        colors.extend(Texels::Srgb8Rgba(pixels))?;
        Ok(colors.finish([unit; 3])?)
    };
    Ok([scan(Coverage::Opaque)?, scan(Coverage::AlphaAtLeast(0.5))?])
}

pub(super) fn map_ranges(mers: &[u8]) -> Result<[openpbr::Range; 3], Error> {
    let unit = openpbr::Range::new(0., 1.)?;
    let mut scans = [openpbr::RangeScan::new(); 3];
    for pixel in mers.as_chunks::<4>().0 {
        let r = f32::from(pixel[2]) / 255.;
        for (scan, value) in
            scans
                .iter_mut()
                .zip([r, f32::from(pixel[0]) / 255., f32::from(pixel[3]) / 255.])
        {
            scan.extend(&[value])?;
        }
    }
    Ok(if mers.is_empty() {
        [unit; 3]
    } else {
        let mut ranges = [unit; 3];
        for (target, scan) in ranges.iter_mut().zip(scans) {
            *target = scan.finish().unwrap_or(unit);
        }
        ranges
    })
}

pub(super) fn flows(
    colors: [openpbr::resource::ColorRanges; 2],
    definitions: &[MaterialDefinition],
    textured: [openpbr::Range; 3],
) -> Result<Vec<openpbr::FlowKey>, Error> {
    use openpbr::resource::MaterialBinding;
    use openpbr::{InterfaceIor, MaterialRanges, Range};
    if definitions.is_empty() || definitions.iter().any(|m| !m.valid()) {
        return Err(Error::Invalid("材质表为空或字段不符合OpenPBR输入域"));
    }
    let mut flows = Vec::new();
    flows
        .try_reserve_exact(definitions.len())
        .map_err(scene_cache::Error::from)?;
    for definition in definitions {
        let texture = definition.control[2] & MaterialDefinition::MERS != 0;
        let roughness = if definition.control[3] & MaterialDefinition::AUTHORED_NORMAL != 0
            && definition.control[2] & MaterialDefinition::AUXILIARY != 0
        {
            // shader将aux分布与roughness合成；未分析局部aux范围时保留全部连续/离散支持。
            Range::new(0., 1.)?
        } else if texture {
            textured[0]
        } else {
            Range::point(definition.base_roughness[3])?
        };
        let ranges = if definition.control[0] & 1 != 0 {
            MaterialRanges::dielectric(roughness, definition.control[0] & 2 != 0)?
        } else {
            MaterialRanges::opaque(
                roughness,
                if texture {
                    textured[1]
                } else {
                    Range::point(definition.surface[1])?
                },
                if texture {
                    textured[2]
                } else {
                    Range::point(definition.surface[2])?
                },
            )?
        };
        let emission = definition.emission[3] > 0.
            && (definition.emission[..3].iter().any(|&v| v > 0.)
                || definition.control[2] & MaterialDefinition::TEXTURED_EMISSION != 0);
        let colors = colors[usize::from(definition.control[2] & MaterialDefinition::CUTOUT != 0)];
        flows.push(MaterialBinding::new(ranges, colors, InterfaceIor::UNKNOWN, emission)?.flow());
    }
    Ok(flows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normal_distribution_keeps_continuous_reflection_in_the_program_support() {
        let mut material = MaterialDefinition::default();
        material.base_roughness[3] = 0.;
        material.control[3] = MaterialDefinition::AUTHORED_NORMAL;
        material.control[2] |= MaterialDefinition::AUXILIARY;
        let flow = flows(
            colors(&[255; 4]).unwrap(),
            &[material],
            map_ranges(&[]).unwrap(),
        )
        .unwrap();
        assert_ne!(flow[0].bits() & openpbr::FlowKey::CONTINUOUS_REFLECTION, 0);
    }
}
