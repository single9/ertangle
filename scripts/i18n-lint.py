#!/usr/bin/env python3
"""檢查一個 .po：沒有 fuzzy、沒有缺譯、每個 msgstr 的 {佔位符} 與 msgid 相同。"""
import re
import sys

PLACEHOLDER = re.compile(r"(?<!\{)\{([^{}]*)\}(?!\})")


def unquote(lines):
    return "".join(
        bytes(l[1:-1], "utf-8").decode("unicode_escape").encode("latin-1").decode("utf-8")
        for l in lines
    )


def entries(path):
    """逐筆回傳 (flags, msgid, msgstr 清單, 起始行號)。過時項目 (#~) 略過。"""
    cur, field, flags, start = {}, None, [], 0
    for no, raw in enumerate(open(path, encoding="utf-8"), 1):
        line = raw.strip()
        if not line:
            if cur:
                yield flags, cur, start
            cur, field, flags = {}, None, []
            continue
        if line.startswith("#,"):
            flags += [f.strip() for f in line[2:].split(",")]
        elif line.startswith("#"):
            continue
        elif line.startswith('"'):
            cur[field].append(line)
        else:
            key, _, rest = line.partition(" ")
            if not cur:
                start = no
            field = key
            cur[field] = [rest]
    if cur:
        yield flags, cur, start


def main(path):
    errors = []
    for flags, e, line in entries(path):
        msgid = unquote(e.get("msgid", ['""']))
        if not msgid:
            continue  # 檔頭
        strs = [unquote(v) for k, v in e.items() if k.startswith("msgstr")]
        where = f"{path}:{line}: {msgid!r}"
        if "fuzzy" in flags:
            errors.append(f"{where}: 標記為 fuzzy，請確認翻譯後移除標記")
        if not strs or any(not s for s in strs):
            errors.append(f"{where}: 缺少翻譯")
            continue
        want = sorted(PLACEHOLDER.findall(msgid))
        for s in strs:
            got = sorted(PLACEHOLDER.findall(s))
            if got != want:
                errors.append(f"{where}: 佔位符不一致，原文 {want}，翻譯 {got}")
    for err in errors:
        print(err, file=sys.stderr)
    return 1 if errors else 0


if __name__ == "__main__":
    sys.exit(max(main(p) for p in sys.argv[1:]))
