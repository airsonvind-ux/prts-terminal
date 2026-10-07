#!/usr/bin/env python3
"""PRTS-MAA 桥的测试客户端（开发用）。

协议走**本机 TCP**（桥作服务端，`--port <n>`）：桥启动监听 127.0.0.1，
本脚本连入后收发帧。读帧放后台线程 + 队列超时，避免阻塞卡死。

用法：
    python bridge_client.py --exe <exe> --maa-dir <MAA> --user-dir <dir>
    python bridge_client.py ... --bogus     # 用不存在的 hwnd，不碰真实窗口
    python bridge_client.py ... --matrix    # 遍历 Win32 截图/输入方式组合
"""
import argparse
import json
import queue
import socket
import struct
import subprocess
import sys
import threading
import time
from pathlib import Path

GAME_TITLE = "明日方舟"


def attach_ret_of(m):
    """从桥转发的 maa 事件里取 AttachWindow 的 ret。

    桥的帧结构：{"details": <MaaCore 载荷>, "raw": ...}，
    而 AttachWindow 载荷形如 {"what":"AttachWindow","details":{"ret":true,...}}，
    所以 ret 在 details.details.ret（不是 details.ret）。
    """
    d = m.get("details") or {}
    inner = d.get("details")
    if isinstance(inner, dict) and "ret" in inner:
        return inner.get("ret")
    return d.get("ret")


def pick_port():
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    port = s.getsockname()[1]
    s.close()
    return port


def recvn(sock, n):
    out = b""
    while len(out) < n:
        chunk = sock.recv(n - len(out))
        if not chunk:
            return None
        out += chunk
    return out


class Bridge:
    def __init__(self, exe, maa_dir, user_dir):
        self.port = pick_port()
        self.p = subprocess.Popen(
            [exe, "--maa-dir", maa_dir, "--user-dir", user_dir, "--port", str(self.port)],
            stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        )
        self.sock = None
        for _ in range(100):
            try:
                self.sock = socket.create_connection(("127.0.0.1", self.port), timeout=1)
                break
            except OSError:
                if self.p.poll() is not None:
                    raise RuntimeError("桥已退出（未监听）")
                time.sleep(0.1)
        if self.sock is None:
            raise RuntimeError("连接桥超时")
        # create_connection 带 1s 超时，连接完成后必须清掉：
        # 否则读线程空闲 1s 就会 recv 超时，被误判成断线。
        self.sock.settimeout(None)
        self.sock.setsockopt(socket.IPPROTO_TCP, socket.TCP_NODELAY, 1)
        self.q = queue.Queue()
        threading.Thread(target=self._reader, daemon=True).start()

    def _reader(self):
        try:
            while True:
                h = recvn(self.sock, 4)
                if h is None:
                    break
                n = struct.unpack(">I", h)[0]
                d = recvn(self.sock, n)
                if d is None:
                    break
                try:
                    self.q.put(json.loads(d.decode("utf-8")))
                except Exception as e:  # noqa: BLE001
                    self.q.put({"event": "parse_error", "error": str(e)})
        except OSError:
            pass
        self.q.put(None)

    def send(self, obj):
        b = json.dumps(obj, ensure_ascii=False).encode("utf-8")
        self.sock.sendall(struct.pack(">I", len(b)) + b)

    def recv(self, timeout):
        try:
            return self.q.get(timeout=timeout)
        except queue.Empty:
            return "TIMEOUT"

    def wait(self, pred, timeout=30, verbose=False):
        end = time.time() + timeout
        while time.time() < end:
            m = self.recv(min(0.5, max(0.05, end - time.time())))
            if m == "TIMEOUT":
                continue
            if m is None:
                print("  <closed>")
                return None
            if verbose:
                if m.get("event") == "maa":
                    print("  [maa]", m.get("code"), m.get("name"), m.get("raw"))
                else:
                    print("  [msg]", json.dumps(m, ensure_ascii=False))
            if pred(m):
                return m
        print("  <timeout>")
        return None

    def wait_result(self, rid, timeout=30, verbose=False):
        return self.wait(
            lambda m: m.get("event") == "result" and m.get("id") == rid,
            timeout,
            verbose,
        )

    def attach_ret(self, hwnd, screencap, mouse, keyboard, timeout=5):
        self.send({
            "cmd": "connect_win32", "id": 100, "hwnd": hwnd,
            "screencap": screencap, "mouse": mouse, "keyboard": keyboard,
        })
        self.wait_result(100, timeout)
        r = self.wait(
            lambda m: m.get("event") == "maa" and m.get("code") == 4
            and (m.get("details") or {}).get("what") == "AttachWindow",
            timeout,
        )
        return None if not r else attach_ret_of(r)

    def close(self):
        try:
            self.send({"cmd": "shutdown", "id": 999})
            self.p.wait(timeout=5)
        except Exception:  # noqa: BLE001
            self.p.kill()
        try:
            self.sock.close()
        except Exception:  # noqa: BLE001
            pass


def png_size(path):
    """从 PNG 文件头读宽高（IHDR 紧跟 8 字节签名 + 4 字节长度 + 4 字节 'IHDR'）。"""
    try:
        with open(path, "rb") as f:
            head = f.read(24)
    except OSError:
        return None
    if head[:8] != b"\x89PNG\r\n\x1a\n":
        return None
    w, h = struct.unpack(">II", head[16:24])
    return w, h


def adb_run(b, args):
    """ADB（模拟器）通道：连接 → 状态/截图 →（可选）一个核心任务。"""
    print(f"ADB 连接：address={args.adb}  adb_path={args.adb_path}", flush=True)
    # 连接 extras（MuMu 截图/触控增强等）：须在 connect **之前**设。
    extras_name = args.extras_name
    extras_json = args.extras_json
    config = args.adb_config
    if args.mumu_path:
        extras_name = "MuMuEmulator12"
        extras_json = json.dumps({
            "path": args.mumu_path, "touch": bool(args.mumu_touch), "client_type": args.mumu_client,
        }, ensure_ascii=False)
        if not config:
            config = "MuMuEmulator12"
    if extras_name:
        b.send({"cmd": "set_connection_extras", "id": 1,
                "name": extras_name, "extras": extras_json or "{}"})
        print("set_connection_extras:", json.dumps(b.wait_result(1, 10), ensure_ascii=False), flush=True)

    frame = {"cmd": "connect_adb", "id": 2, "adb_path": args.adb_path, "address": args.adb}
    if config:
        frame["config"] = config
    b.send(frame)
    r = b.wait_result(2, 15)
    print("connect_adb:", json.dumps(r, ensure_ascii=False), flush=True)
    if not r or not r.get("ok"):
        print("connect_adb 未被受理，终止", flush=True)
        b.close()
        return 1

    # 等连接结果：以 AsyncCallInfo(码 4, what=Connect) 的 ret 为准。
    # 不能用 ConnectionInfo 的 Connected 判定——它早于控制单元初始化完成
    # （还要给截图方式测速、跑 minitouch probe），据此判断会过早。
    connected = None
    end = time.time() + args.connect_timeout
    while time.time() < end:
        m = b.recv(1)
        if m is None:
            print("<closed>", flush=True)
            break
        if m == "TIMEOUT":
            continue
        ev = m.get("event")
        if ev == "maa":
            code = m.get("code")
            what = (m.get("details") or {}).get("what")
            if code == 2:  # ConnectionInfo：过程信息，只打印
                inner = (m.get("details") or {}).get("details")
                extra = "" if inner is None else " " + json.dumps(inner, ensure_ascii=False)
                print(f"  [conn] {what}{extra}", flush=True)
                if what in ("ConnectFailed", "UnsupportedResolution", "EmulatorNotSupported",
                            "UnknownError", "InternalError", "ConnectionFailed"):
                    connected = False
                    break
            elif code == 4 and what == "Connect":  # AsyncCallInfo：连接真正结束
                ret = attach_ret_of(m)
                print(f"  [async] Connect ret={ret}", flush=True)
                connected = bool(ret) if ret is not None else None
                break
        elif ev == "error":
            print("  [error]", json.dumps(m, ensure_ascii=False), flush=True)
    print("connected:", connected, flush=True)

    # 连接确定后轮询 status 直到 connected（控制单元可能还差一拍）
    st = None
    if connected:
        for _ in range(20):
            b.send({"cmd": "status", "id": 3})
            st = b.wait_result(3, 5)
            if st and st.get("data", {}).get("connected"):
                break
            time.sleep(0.25)
    print("status:", json.dumps(st, ensure_ascii=False), flush=True)

    if connected:
        b.send({"cmd": "screenshot", "id": 4})
        shot = b.wait_result(4, 20)
        print("screenshot:", json.dumps(shot, ensure_ascii=False), flush=True)
        if shot and shot.get("ok"):
            p = shot.get("data", {}).get("path")
            sz = png_size(p) if p else None
            if sz:
                ratio = sz[0] / sz[1] if sz[1] else 0
                tag = "16:9 ✓" if abs(ratio - 16 / 9) < 0.01 else "非 16:9 ⚠"
                print(f"  帧尺寸: {sz[0]}×{sz[1]}  ({tag})", flush=True)

    if args.connect_only or not connected:
        b.close()
        return 0 if connected else 1

    # 核心任务
    params = json.loads(args.task_params) if args.task_params else {}
    if args.task_client:
        params["client_type"] = args.task_client
    b.send({"cmd": "task_add", "id": 5, "type": args.task, "params": params})
    ta = b.wait_result(5, 10)
    print("task_add:", json.dumps(ta, ensure_ascii=False), flush=True)
    if not ta or not ta.get("ok"):
        print("任务未被接受，终止", flush=True)
        b.close()
        return 1
    b.send({"cmd": "run_start", "id": 6})
    print("run_start:", json.dumps(b.wait_result(6, 10), ensure_ascii=False), flush=True)

    end = time.time() + args.task_timeout
    maa_count = 0
    done = False
    while time.time() < end:
        m = b.recv(2)
        if m == "TIMEOUT":
            continue
        if m is None:
            print("<closed>", flush=True)
            break
        ev = m.get("event")
        if ev == "recognition":
            det = m.get("details") or {}
            all_n = len(det.get("all_opers") or [])
            own_n = len(det.get("own_opers") or [])
            extra = f" all={all_n} own={own_n}" if (all_n or own_n) else ""
            print(f"  [recognition] kind={m.get('kind')} done={det.get('done')}{extra}", flush=True)
            if det.get("done"):
                out = Path(args.user_dir) / "recon" / f"{m.get('kind')}.json"
                out.parent.mkdir(parents=True, exist_ok=True)
                out.write_text(json.dumps(m, ensure_ascii=False, indent=2), encoding="utf-8")
                print(f"    完整结果已落盘: {out}", flush=True)
        elif ev == "maa":
            maa_count += 1
            code = m.get("code")
            if code in (10000, 20000):
                print("  [maa-error]", m.get("name"), str(m.get("raw"))[:200], flush=True)
            elif code == 3:  # AllTasksCompleted
                print("  [maa] AllTasksCompleted", flush=True)
                done = True
                break
        elif ev == "error":
            print("  [error]", json.dumps(m, ensure_ascii=False), flush=True)
    print("maa 事件计数:", maa_count, " 完成:", done, flush=True)
    b.send({"cmd": "run_stop", "id": 7})
    print("run_stop:", json.dumps(b.wait_result(7, 10), ensure_ascii=False), flush=True)
    b.close()
    return 0


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--exe", required=True)
    ap.add_argument("--maa-dir", required=True)
    ap.add_argument("--user-dir", required=True)
    ap.add_argument("--bogus", action="store_true", help="用不存在的 hwnd，不碰真实窗口")
    ap.add_argument("--surface", action="store_true", help="只验证命令面（不碰窗口、不连接）")
    ap.add_argument("--probe-types", action="store_true", help="只验类型接受度：逐个 task_add（不连接）")
    ap.add_argument("--recognize", action="store_true", help="attach + 跑只读识别任务，看 recognition 事件")
    ap.add_argument("--startup", action="store_true", help="attach + 跑最小 StartUp 任务，验证能否认出主界面")
    ap.add_argument("--matrix", action="store_true", help="遍历截图/输入方式组合")
    ap.add_argument("--windump", action="store_true", help="只读窗口几何（外框/客户区/DPI），不连接、不改窗口")
    ap.add_argument("--screencap", type=int, default=16, help="截图方式掩码（默认 16=PrintWindow；27=GDI|FramePool|DXGIWin|PrintWindow）")
    ap.add_argument("--mouse", type=int, default=256, help="鼠标输入掩码（默认 256=PostMsg-WindowPos；1024=AnchoredTouch；64=PostMsg-CursorPos；1=Seize）")
    ap.add_argument("--keyboard", type=int, default=2, help="键盘输入掩码（默认 2=SendMessage）")
    ap.add_argument("--adb", metavar="ADDR", help="ADB 地址（如 127.0.0.1:16384）；给则走 ADB（模拟器）通道，忽略 Win32 选项")
    ap.add_argument("--adb-path", default="adb", help="adb 可执行文件路径（默认用 PATH 里的 adb）")
    ap.add_argument("--adb-config", help="ADB 连接 config（如 MuMuEmulator12，或 JSON 串）；不给则用 MAA 默认")
    ap.add_argument("--extras-name", help="ADB：连接 extras 名（如 MuMuEmulator12），须在 connect 之前设")
    ap.add_argument("--extras-json", help="ADB：连接 extras 的 JSON 字符串")
    ap.add_argument("--mumu-path", help="ADB：MuMu 安装目录；给则自动设 MuMuEmulator12 extras 并把 connect config 设为它")
    ap.add_argument("--mumu-touch", action="store_true", help="ADB：--mumu-path 时同时启用触控增强（touch=true）")
    ap.add_argument("--mumu-client", default="Official", help="ADB：--mumu-path 时的客户端类型（Official / Bilibili / …）")
    ap.add_argument("--connect-only", action="store_true", help="ADB：只验证连接 + 截图，不跑任务")
    ap.add_argument("--task", default="StartUp", help="ADB：核心任务类型（默认 StartUp）")
    ap.add_argument("--task-params", help="ADB：任务参数 JSON（留空 = {}）")
    ap.add_argument("--task-client", help="ADB：注入 task-params.client_type（绕开 shell 传 JSON 的引号坑）")
    ap.add_argument("--connect-timeout", type=float, default=60, help="ADB：连接等待秒数")
    ap.add_argument("--task-timeout", type=float, default=300, help="ADB：任务等待秒数")
    args = ap.parse_args()

    b = Bridge(args.exe, args.maa_dir, args.user_dir)
    print("READY:", json.dumps(b.recv(60), ensure_ascii=False))

    if args.surface:
        steps = [
            ("task_add(OperBox)", {"cmd": "task_add", "id": 10, "type": "OperBox", "params": {}}),
            ("task_add(Fight)", {"cmd": "task_add", "id": 11, "type": "Fight", "params": {"stage": "CE-6"}}),
            ("task_list", {"cmd": "task_list", "id": 12}),
            ("task_set(#1)", {"cmd": "task_set", "id": 13, "task_id": 1, "params": {"times": 2}}),
            ("task_list again", {"cmd": "task_list", "id": 14}),
            ("set_instance_option", {"cmd": "set_instance_option", "id": 15, "key": 2, "value": "minitouch"}),
            ("uuid", {"cmd": "uuid", "id": 16}),
            ("status", {"cmd": "status", "id": 17}),
        ]
        for label, frame_cmd in steps:
            b.send(frame_cmd)
            r = b.wait_result(frame_cmd["id"], 10)
            print(f"  {label:20} -> {json.dumps(r, ensure_ascii=False)}")
        b.close()
        return 0

    if args.probe_types:
        # 类型接受度实测：**不连接**，逐个 task_add（params={}），看返回码。
        # 0 是二义的（类型错 / 参数错都回 0），用 MaaCore 的 asst.log
        #（"invalid type" / "invalid params"）区分。
        types = [
            "StartUp", "CloseDown", "Fight", "Recruit", "Infrast", "Mall", "Award",
            "Roguelike", "Copilot", "SSSCopilot", "ParadoxCopilot", "Reclamation",
            "SingleStep", "VideoRecognition", "Depot", "OperBox", "OperProgress",
            "SwitchTheme", "Custom",
            "DefinitelyNotATask",  # 故意的非法类型：验证“回 0”确实走的是“类型未识别”
        ]
        print(f"{'#':>2}  {'type':22} {'ok':5} {'task_id':>7}  detail")
        for i, t in enumerate(types, 1):
            rid = 1000 + i
            b.send({"cmd": "task_add", "id": rid, "type": t, "params": {}})
            r = b.wait_result(rid, 15)
            if r is None:
                print(f"{i:>2}  {t:22} <no result>")
                continue
            ok = r.get("ok")
            data = r.get("data") or {}
            tid = data.get("task_id")
            detail = "" if ok else (r.get("error") or data.get("error") or "")
            print(f"{i:>2}  {t:22} {str(ok):5} {str(tid):>7}  {detail}")
        b.close()
        return 0

    if args.adb:
        try:
            return adb_run(b, args)
        except Exception as e:  # noqa: BLE001
            print("ADB 模式异常:", e, flush=True)
            b.close()
            return 1

    if args.bogus:
        hwnd = 1
        print("HWND: 1 (bogus，不碰真实窗口)")
    else:
        b.send({"cmd": "find_window", "id": 1, "title": GAME_TITLE})
        fw = b.wait_result(1, verbose=False)
        if not fw or not fw.get("data", {}).get("hwnd"):
            print("未找到游戏窗口")
            b.close()
            return 1
        hwnd = fw["data"]["hwnd"]
        print("HWND:", hwnd, "class:", fw["data"].get("class"))

    if args.windump:
        b.send({"cmd": "window_info", "id": 20, "hwnd": hwnd})
        print("window_info:", json.dumps(b.wait_result(20, 10), ensure_ascii=False), flush=True)
        b.close()
        return 0

    if args.recognize:
        # 对齐 MAA 的 PC 端设置：截图 PrintWindow(16) / 鼠标 PostMessageWithWindowPos(256) / 键盘 SendMessage(2)
        b.send({"cmd": "connect_win32", "id": 2, "hwnd": hwnd,
                "screencap": args.screencap, "mouse": args.mouse, "keyboard": args.keyboard})
        # 直接收帧：回复与 attach 异步结果可能交错，一次收齐
        got_res = False
        attach_ret = None
        end = time.time() + 20
        while time.time() < end and not (got_res and attach_ret is not None):
            m = b.recv(1)
            if m == "TIMEOUT" or m is None:
                continue
            if m.get("event") == "result" and m.get("id") == 2:
                got_res = True
            elif (m.get("event") == "maa" and m.get("code") == 4
                  and (m.get("details") or {}).get("what") == "AttachWindow"):
                attach_ret = attach_ret_of(m)
        print("attach ret:", attach_ret, flush=True)
        if attach_ret is not True:
            print("attach 未成功，终止（不再起任务，避免 MaaCore 空控制器崩溃）", flush=True)
            b.close()
            return 1
        b.send({"cmd": "task_add", "id": 3, "type": "OperBox", "params": {}})
        print("task_add:", json.dumps(b.wait_result(3, 10), ensure_ascii=False), flush=True)
        b.send({"cmd": "run_start", "id": 4})
        print("run_start:", json.dumps(b.wait_result(4, 10), ensure_ascii=False), flush=True)

        end = time.time() + 300
        maa_count = 0
        final = None
        while time.time() < end:
            m = b.recv(2)
            if m == "TIMEOUT":
                continue
            if m is None:
                print("<closed>", flush=True)
                break
            ev = m.get("event")
            if ev == "recognition":
                det = m.get("details") or {}
                print(f"  [recognition] kind={m.get('kind')} done={det.get('done')} "
                      f"all={len(det.get('all_opers') or [])} own={len(det.get('own_opers') or [])}", flush=True)
                if m.get("kind") == "OperBoxInfo" and det.get("done"):
                    final = m
                    break
            elif ev == "maa":
                maa_count += 1
                if m.get("code") in (10000, 20000):
                    print("  [maa-error]", m.get("name"), str(m.get("raw"))[:200], flush=True)
            elif ev == "error":
                print("  [error]", json.dumps(m, ensure_ascii=False), flush=True)
        print("maa 事件计数:", maa_count, flush=True)

        out = Path(args.user_dir) / "recon" / "operboxinfo_live.json"
        out.parent.mkdir(parents=True, exist_ok=True)
        if final:
            out.write_text(json.dumps(final, ensure_ascii=False, indent=2), encoding="utf-8")
            det = final.get("details") or {}
            print("完整结果已落盘:", out, flush=True)
            print("  all_opers:", len(det.get("all_opers") or []),
                  "own_opers:", len(det.get("own_opers") or []), flush=True)
        else:
            print("未拿到 done:true 的完整结果", flush=True)
        b.send({"cmd": "run_stop", "id": 5})
        print("run_stop:", json.dumps(b.wait_result(5, 10), ensure_ascii=False), flush=True)
        b.close()
        return 0

    if args.startup:
        # 对齐 MAA 的 PC 端设置：截图 PrintWindow(16) / 鼠标 PostMessageWithWindowPos(256) / 键盘 SendMessage(2)
        b.send({"cmd": "connect_win32", "id": 2, "hwnd": hwnd,
                "screencap": args.screencap, "mouse": args.mouse, "keyboard": args.keyboard})
        b.wait_result(2, 10)
        aw = b.wait(
            lambda m: m.get("event") == "maa" and m.get("code") == 4
            and (m.get("details") or {}).get("what") == "AttachWindow", 30)
        attach_ret = None if not aw else attach_ret_of(aw)
        print("attach ret:", attach_ret, flush=True)
        if attach_ret is not True:
            print("attach 未成功，终止", flush=True)
            b.close()
            return 1
        b.send({"cmd": "task_add", "id": 3, "type": "StartUp", "params": {"client_type": "Official"}})
        r3 = b.wait_result(3, 10)
        print("task_add:", json.dumps(r3, ensure_ascii=False), flush=True)
        if not r3 or not r3.get("ok"):
            print("StartUp 未被接受，终止", flush=True)
            b.close()
            return 1
        b.send({"cmd": "run_start", "id": 4})
        print("run_start:", json.dumps(b.wait_result(4, 10), ensure_ascii=False), flush=True)
        end = time.time() + 120
        while time.time() < end:
            m = b.recv(2)
            if m == "TIMEOUT":
                continue
            if m is None:
                print("<closed>", flush=True)
                break
            ev = m.get("event")
            if ev == "maa":
                code = m.get("code")
                if code in (1, 3, 10000, 10001, 10002, 10004, 20000):
                    print("  [maa]", code, m.get("name"), str(m.get("raw"))[:220], flush=True)
                if code in (3, 10002, 10000):
                    break
            elif ev == "recognition":
                print("  [recognition]", m.get("kind"), flush=True)
            elif ev == "error":
                print("  [error]", json.dumps(m, ensure_ascii=False), flush=True)
        b.send({"cmd": "run_stop", "id": 5})
        print("run_stop:", json.dumps(b.wait_result(5, 10), ensure_ascii=False), flush=True)
        b.close()
        return 0

    if args.matrix:
        screencaps = {"GDI": 1, "FramePool": 2, "DXGIDup": 4, "DXGIDupWin": 8, "PrintWindow": 16, "ScreenDC": 32}
        mice = {"Seize": 1, "SendMsg": 2, "PostMsg": 4, "SendMsgCursor": 32, "PostMsgCursor": 64,
                "SendMsgWin": 128, "PostMsgWin": 256, "AnchoredTouch": 1024}
        hit = None
        for sn, sv in screencaps.items():
            for mn, mv in mice.items():
                ret = b.attach_ret(hwnd, sv, mv, 2)
                print(f"  {sn:13} {mn:15} -> ret={ret}")
                if ret:
                    hit = (sn, sv, mn, mv)
                    print("  *** SUCCESS ***")
                    break
            if hit:
                break
        if hit:
            print("可用组合:", hit)
        b.close()
        return 0

    b.send({"cmd": "connect_win32", "id": 2, "hwnd": hwnd})
    b.wait_result(2, timeout=10, verbose=True)
    time.sleep(1)
    b.send({"cmd": "status", "id": 3})
    print("status:", json.dumps(b.wait_result(3, timeout=10, verbose=True), ensure_ascii=False))
    b.send({"cmd": "screenshot", "id": 4})
    print("shot:", json.dumps(b.wait_result(4, timeout=10, verbose=True), ensure_ascii=False))
    b.close()
    return 0


if __name__ == "__main__":
    sys.exit(main())
