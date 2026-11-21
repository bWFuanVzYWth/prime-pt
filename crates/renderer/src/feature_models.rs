//! 一个资源世代的刚性模型组件。源 UV、绕序及索引在资源准备时固定。
use super::*;
use feature_source::{read, take};
pub(super) struct Catalog {
    pub mesh: Vec<(u32, u32, vulkan::TriangleBlas)>,
    pub vertices: Buffer,
}
impl Catalog {
    pub fn prepare(ctx: &mut Context, bytes: &[u8], placeholder: Buffer) -> Result<Self, Error> {
        let mut input = bytes;
        let [tag, count] = read::<[u32; 2]>(&mut input)?;
        if tag != u32::from_le_bytes(*b"PFM1") || count > 100000 {
            return Err(Error::Invalid("模型部件目录无效"));
        }
        let mut tris = Vec::new();
        let mut vertices = Vec::new();
        let mut ranges = Vec::new();
        for _ in 0..count {
            let n = read::<u32>(&mut input)?;
            if n == 0 || n > 65536 || vertices.len() / 4 + n as usize > 2_000_000 {
                return Err(Error::Invalid("模型面的数量无效"));
            }
            let bytes = take(&mut input, n as usize * 80)?;
            let start = (vertices.len() / 4) as u32;
            ranges.push((start, n));
            for quad in bytes.chunks_exact(80) {
                let v: [[f32; 5]; 4] = bytemuck::pod_read_unaligned(quad);
                if !v.iter().flatten().all(|x| x.is_finite()) {
                    return Err(Error::Invalid("模型顶点非有限"));
                }
                let p = v.map(|[x, y, z, _, _]| [x, y, z]);
                tris.extend([[p[0], p[1], p[2]], [p[0], p[2], p[3]]]);
                vertices.extend(v.map(|[x, y, z, u, v]| [x, y, z, u, v, 0., 0., 0.]));
            }
        }
        if !input.is_empty() {
            return Err(Error::Invalid("模型目录尾部多余数据"));
        }
        let inputs: Vec<_> = ranges
            .iter()
            .map(|&(s, n)| vulkan::TriangleBlasInput {
                triangles: &tris[(s * 2) as usize..((s + n) * 2) as usize],
                opaque: false,
                opacity: None,
            })
            .collect();
        let mesh = ctx
            .triangle_blas_batch(&inputs)?
            .blases
            .into_iter()
            .zip(ranges)
            .map(|(b, (s, n))| (s, n, b))
            .collect();
        let vertices = if vertices.is_empty() {
            placeholder
        } else {
            Buffer::upload(ctx, bytemuck::cast_slice(&vertices))?
        };
        Ok(Self { mesh, vertices })
    }
    pub fn roots(&self, r: &mut Vec<vulkan::native::Resource>) {
        r.push(self.vertices.into());
        r.extend(
            self.mesh
                .iter()
                .map(|q| vulkan::native::Resource::from(q.2)),
        );
    }
}
