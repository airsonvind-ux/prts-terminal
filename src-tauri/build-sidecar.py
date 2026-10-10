"""把 MAA 桥编成 Tauri 的 sidecar 形态，供打包时随 PRTS 分发。

用法（项目根目录）：
    python src-tauri/build-sidecar.py

为什么需要这一步：`tauri.conf.json` 的 `bundle.externalBin` 声明了
`binaries/prts-maa-bridge`，Tauri 要求该文件按 `<名>-<目标三元组>.exe` 命名，
并且**在编译期就存在**（构建脚本会校验，缺文件连 `cargo test` 都过不去）。

桥是独立 crate（`bridge/`），运行时链接用户 MAA 目录里的 MaaCore（AGPL 组件），
只经本机 TCP 与 PRTS 通信，所以必须以独立可执行文件随包分发，不能静态链接进主程序。

输出：`src-tauri/binaries/prts-maa-bridge-<三元组>.exe`（构建产物，不入库；
`src-tauri/binaries/` 已在 .gitignore）。
"""

from pathlib import Path
import shutil
import subprocess
import sys


def host_triple() -> str:
    """问 rustc 要目标三元组；取不到时按 Windows x64 兜底。"""
    try:
        info = subprocess.run(
            ["rustc", "-vV"], capture_output=True, text=True, check=True
        ).stdout
        for line in info.splitlines():
            if line.startswith("host: "):
                return line.split(": ", 1)[1].strip()
    except Exception:
        pass
    return "x86_64-pc-windows-msvc"


def main() -> int:
    here = Path(__file__).resolve()
    project_root = here.parents[1]        # src-tauri/build-sidecar.py -> 项目根
    bridge_dir = project_root / "bridge"
    out_dir = here.parent / "binaries"    # src-tauri/binaries

    win = sys.platform == "win32"
    exe_name = "prts-maa-bridge.exe" if win else "prts-maa-bridge"
    dest = out_dir / f"prts-maa-bridge-{host_triple()}{'.exe' if win else ''}"

    print(f"· 编译桥（release）：{bridge_dir}")
    result = subprocess.run(
        ["cargo", "build", "--release", "--manifest-path", str(bridge_dir / "Cargo.toml")]
    )
    if result.returncode != 0:
        print("桥编译失败，已中止", file=sys.stderr)
        return result.returncode

    src = bridge_dir / "target" / "release" / exe_name
    if not src.is_file():
        print(f"找不到构建产物：{src}", file=sys.stderr)
        return 1

    out_dir.mkdir(parents=True, exist_ok=True)
    shutil.copy2(src, dest)
    print(f"· sidecar 就位：{dest.name}（{src.stat().st_size} 字节）")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
