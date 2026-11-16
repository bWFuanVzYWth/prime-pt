/// CPU 上传的单帧相机和绝对样本范围。
module params;
import pt.scene;
import display.prime_drt;
import atmosphere.parameters;
import geometry.camera;

public struct Params
{
    public var origin : float4;
    public var forward : float4;
    public var right : float4;
    public var up : float4;
    /// xy=尺寸、z=批次起点、w=样本数。
    public var image : uint4;
    /// seed、最大反弹次数、Sobol log2分辨率、dielectric 上限。
    public var options : uint4;
    public var drt : display::PrimeDrtParameters;
    /// xyz=朝向太阳，w=正值表示启用天空；强度由大气辐亮度倍率单独控制。
    public var lighting : float4;
    public var atmosphere : atmosphere::Params;
    /// Rec.2020 辐亮度倍率与 Aerial 最远距离 km。
    public var atmosphere_settings : float4;
    /// xy=全帧主射线偏移，z=历史有效，w=0原生、2 RR。
    public var reconstruction : float4;
    /// 本批绝对随机样本基数；与静态累积count独立，失效不回退。
    public var rr_sample : uint;
    /// 方块发光的场景线性倍率，命中和直接光估计使用同一个值。
    public var block_emission_scale : float;
    /// 正有限的倒输入尺寸，由宿主按本帧实际追踪尺寸准备。
    public var inverse_extent : float2;
    public var previous_position : float4;
    public var previous_forward : float4;
    public var previous_right : float4;
    public var previous_up : float4;
    /// xy=输出尺寸；z bit0诊断、bit2作者法线、bit5实时首面策略；w非零启用质量档平方根轮盘。
    public var output_size : uint4;
    /// 当前64m anchor的整数region坐标；光源保持region-local不随anchor重打包。
    public var anchor_region : int4;
    /// 宿主实际相机介质的IOR和RGB extinction。
    public var initial_medium : float4;
    /// xy为输入/输出尺寸比例bits，z为兼容导引偏移，w为同源太阳半角的弦长平方bits。
    public var programs : uint4;
    /// 当前规范相机，源自与previous相同的CPU CameraFrame编码，不含jitter。
    public var current_camera : geometry::Camera;
};

public func settings(params: Params)->pt::Settings
{
    return { params.options.x, params.options.y, params.options.z, params.options.w,
             params.output_size.w != 0 };
}
