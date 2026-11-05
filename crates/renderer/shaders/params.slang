/// CPU 上传的单帧相机和绝对样本范围。
module params;
import pt.scene;
import display.prime_drt;
import atmosphere.parameters;

public struct Params
{
    public float4 origin;
    public float4 forward;
    public float4 right;
    public float4 up;
    /// xy=尺寸、z=批次起点、w=样本数。
    public uint4 image;
    /// seed、顶点预算、Sobol log2分辨率、dielectric 上限。
    public uint4 options;
    public display::PrimeDrtParameters drt;
    /// xyz=朝向太阳，w=正值表示启用天空；强度由大气辐亮度倍率单独控制。
    public float4 lighting;
    public atmosphere::Params atmosphere;
    /// Rec.2020 辐亮度倍率与 Aerial 最远距离 km。
    public float4 atmosphere_settings;
    /// xy=全帧主射线偏移，z=历史有效，w=RR 开启。
    public float4 reconstruction;
    /// RR或原生诊断的绝对随机样本，诊断不读跨帧计数。
    public uint rr_sample;
    /// 方块发光的场景线性倍率，命中和直接光估计使用同一个值。
    public float block_emission_scale;
    public uint2 temporal_reserved;
    public float4 previous_position;
    public float4 previous_forward;
    public float4 previous_right;
    public float4 previous_up;
    /// xy=输出尺寸；z bit0为原生无累积诊断、bit1为ReSTIR、bit2为资源含作者法线；w为历史M cap。
    public uint4 output_size;
    /// 当前64m anchor的整数region坐标；光源保持region-local不随anchor重打包。
    public int4 anchor_region;
    /// 宿主实际相机介质的IOR和RGB extinction。
    public float4 initial_medium;
    /// xy为ReSTIR前帧jitter，z为callable数量的uint位模式，w为fresh输出概率。
    public float4 restir;
};

public pt::Settings settings(Params params)
{
    return { params.options.x, params.options.y, params.options.z, params.options.w };
}
