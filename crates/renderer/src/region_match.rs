//! 两份有序目录单次合并；避免对同一region反复二分旧目录。
use super::{Error, RegionGpu};

pub(super) struct Matched {
    pub reuse: Option<usize>,
    pub attributes_dirty: bool,
}

pub(super) fn regions(
    source: &[prime_scene::RenderRegion<'_>],
    current: &[RegionGpu],
    attributes: &[([i32; 3], u64)],
) -> Result<Vec<Matched>, Error> {
    let mut result = Vec::new();
    result
        .try_reserve_exact(source.len())
        .map_err(scene_cache::Error::from)?;
    let mut old = 0;
    let mut attr = 0;
    for region in source {
        while old < current.len() && current[old].key < region.key {
            old += 1;
        }
        while attr < attributes.len() && attributes[attr].0 < region.key {
            attr += 1;
        }
        let reuse = current
            .get(old)
            .filter(|v| v.key == region.key && v.revision == region.geometry_revision)
            .map(|_| old);
        let attributes_dirty = attributes
            .get(attr)
            .is_none_or(|v| v.0 != region.key || v.1 != region.content_revision);
        result.push(Matched {
            reuse,
            attributes_dirty,
        });
    }
    Ok(result)
}
