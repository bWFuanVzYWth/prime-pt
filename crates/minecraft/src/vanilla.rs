//! 原版明确sprite ID对应的冻结材质数据；源锁与Mojang声明保留于assets。
use crate::{Error, ImageSource};
use std::io::Cursor;
pub(crate) struct Entry {
    name: &'static str,
    game_version: u32,
    extent: [u32; 2],
    mers_png: &'static [u8],
    auxiliary_png: Option<&'static [u8]>,
    pub authored_normal: bool,
}
impl Entry {
    const fn new(
        name: &'static str,
        game_version: u32,
        extent: [u32; 2],
        mers_png: &'static [u8],
        auxiliary_png: Option<&'static [u8]>,
        authored_normal: bool,
    ) -> Self {
        Self {
            name,
            game_version,
            extent,
            mers_png,
            auxiliary_png,
            authored_normal,
        }
    }
    pub(crate) fn images(&self) -> Result<(ImageSource, Option<ImageSource>), Error> {
        let mers = decode(self.mers_png)?;
        let aux = self.auxiliary_png.map(decode).transpose()?;
        if [mers.width, mers.height] != self.extent
            || aux
                .as_ref()
                .is_some_and(|v| [v.width, v.height] != self.extent)
        {
            return Err(Error::Input("内嵌材质PNG尺寸与冻结表不符"));
        }
        Ok((mers, aux))
    }
}
include!("../assets/vanilla_pbr/registry.rs");
pub(crate) fn find(name: &str) -> Option<&'static Entry> {
    PRESETS
        .binary_search_by(|e| (e.name, e.game_version).cmp(&(name, 263)))
        .or_else(|_| PRESETS.binary_search_by(|e| (e.name, e.game_version).cmp(&(name, 0))))
        .ok()
        .map(|i| &PRESETS[i])
}
fn decode(bytes: &[u8]) -> Result<ImageSource, Error> {
    let mut reader = png::Decoder::new(Cursor::new(bytes)).read_info()?;
    let info = reader.info();
    if info.bit_depth != png::BitDepth::Eight
        || info.color_type != png::ColorType::Rgba
        || info.width > 16384
        || info.height > 16384
    {
        return Err(Error::Input("内嵌材质要求有界RGBA8 PNG"));
    }
    let mut pixels = vec![
        0;
        reader
            .output_buffer_size()
            .ok_or(Error::Input("PNG输出尺寸溢出"))?
    ];
    let frame = reader.next_frame(&mut pixels)?;
    pixels.truncate(frame.buffer_size());
    Ok(ImageSource {
        width: frame.width,
        height: frame.height,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_frozen_preset_decodes_with_its_declared_sheet_and_layout() {
        assert!(
            PRESETS
                .windows(2)
                .all(|entries| (entries[0].name, entries[0].game_version)
                    < (entries[1].name, entries[1].game_version))
        );
        for entry in &PRESETS {
            let (image, _) = entry
                .images()
                .unwrap_or_else(|error| panic!("{}: {error}", entry.name));
            crate::vanilla_frames::layout(entry.name, [image.width, image.height])
                .unwrap_or_else(|error| panic!("{}: {error}", entry.name));
        }
    }
}
