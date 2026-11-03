"""PRTS 插件示例（Python）。

宿主与插件用一条 stdio 管道说话：每一帧是
`Content-Length: <字节数>\r\n\r\n<UTF-8 JSON>`。
这里只做三件事：回 initialize、应答一个工具、收到 shutdown 后退出。

改这里就能做自己的插件：把 TOOLS 换成你的工具，把 handle_tool 里的分支
换成你的实现。协议细节见随包手册 05-protocol。
"""

import json
import sys

PROTOCOL = 1

TOOLS = [
    {
        "name": "hello",
        "description": "向一个人打招呼（示例工具）",
        "parameters": {
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "要打招呼的人，默认「世界」"}
            },
            "required": [],
        },
    }
]


def read_frame(stream):
    """读一帧。返回 None 表示对方关了管道。"""
    length = None
    while True:
        line = stream.readline()
        if not line:
            return None
        line = line.strip()
        if not line:
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":", 1)[1].strip())
    if length is None:
        return None
    body = stream.read(length)
    if len(body) != length:
        return None
    return json.loads(body.decode("utf-8"))


def write_frame(stream, message):
    body = json.dumps(message, ensure_ascii=False).encode("utf-8")
    stream.write(b"Content-Length: %d\r\n\r\n" % len(body))
    stream.write(body)
    stream.flush()


def ok(request_id, result):
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def error(request_id, code, message):
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


def handle_tool(name, args):
    if name == "hello":
        who = args.get("name") or "世界"
        return "你好，%s！" % who
    # 工具名不认识按参数错处理（-32602），-32601 只留给方法不认识。
    raise KeyError(name)


def main():
    stdin = sys.stdin.buffer
    stdout = sys.stdout.buffer

    while True:
        message = read_frame(stdin)
        if message is None:
            return 0

        method = message.get("method")
        request_id = message.get("id")

        # shutdown 可能带 id（宿主把它当请求），也可能只是通知；两种都收场。
        if method == "shutdown":
            if request_id is not None:
                write_frame(stdout, ok(request_id, None))
            return 0

        # 其余通知没有 id，不需要回。
        if request_id is None:
            continue

        if method == "initialize":
            write_frame(
                stdout,
                ok(
                    request_id,
                    {
                        "protocol": PROTOCOL,
                        "capabilities": [],
                        "tools": TOOLS,
                    },
                ),
            )
        elif method == "tool.call":
            params = message.get("params") or {}
            try:
                text = handle_tool(params.get("name"), params.get("args") or {})
                write_frame(stdout, ok(request_id, text))
            except KeyError:
                write_frame(
                    stdout,
                    error(
                        request_id,
                        -32602,
                        "没有这个工具：%s" % params.get("name"),
                    ),
                )
        else:
            write_frame(stdout, error(request_id, -32601, "未知方法：%s" % method))


if __name__ == "__main__":
    sys.exit(main())
