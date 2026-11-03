"""从已冻结的原版PBR manifest离线提取帧布局；构建/运行不读取manifest。"""
from pathlib import Path
import argparse
import hashlib
import json


def generate(source: bytes) -> str:
    manifest = json.loads(source)
    if manifest["schema"] != 1:
        raise ValueError("需要原版PBR manifest schema1")
    records = []
    for entry in manifest["entries"]:
        if entry["game_version"] not in (0, 263):
            continue
        animation = entry.get("version_matches", {}).get("263", {}).get("animation", entry["animation"])
        sheet, frame = entry["extent"], entry["frame_extent"]
        if frame == sheet and not animation:
            continue
        if any(value <= 0 or value > 16384 for value in sheet + frame) or any(s % f for s, f in zip(sheet, frame)):
            raise ValueError("非法sheet/frame布局：" + entry["java_id"])
        sequence = [] if not animation else animation["frames"]
        count = sheet[0] // frame[0] * (sheet[1] // frame[1])
        if any(f < 0 or f >= count or duration <= 0 for f, duration in sequence):
            raise ValueError("非法帧序列：" + entry["java_id"])
        records.append((entry["java_id"], sheet, frame, sequence, bool(animation and animation["interpolate"])))
    records.sort(key=lambda row: row[0])
    if len({row[0] for row in records}) != len(records):
        raise ValueError("26.3帧布局身份重复")
    lines = ["// 冻结自原版PBR manifest schema1；运行时不解析JSON。", "// manifest SHA256: " + hashlib.sha256(source).hexdigest(), "#[rustfmt::skip]", f"static FROZEN: [Frozen; {len(records)}] = ["]
    for name, sheet, frame, sequence, interpolate in records:
        lines.append(f"    Frozen {{ name: {json.dumps(name)}, sheet: {sheet}, frame: {frame}, sequence: &{sequence}, interpolate: {str(interpolate).lower()} }},")
    lines.append("];\n")
    return "\n".join(lines)


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("manifest", type=Path)
    parser.add_argument("--output", type=Path, default=Path(__file__).resolve().parents[1] / "crates/minecraft/assets/vanilla_pbr/frames.rs")
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    expected = generate(args.manifest.read_bytes()).encode("utf-8")
    if args.check:
        if args.output.read_bytes() != expected:
            raise ValueError("冻结帧表与输入manifest不一致")
    else:
        args.output.write_bytes(expected)
    print(f"帧表已{'核验' if args.check else '生成'}：{args.output}")


if __name__ == "__main__":
    main()
