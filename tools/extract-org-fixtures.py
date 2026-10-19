#!/usr/bin/env python3
"""Turn org-mode's testing/lisp/test-org-table.el into JSON fixtures.

Every `(org-test-table-target-expect TARGET EXPECT LAPS TBLFM...)` call is
read as data (a tiny s-expression reader; nothing from org-mode is executed)
and becomes one fixture per TBLFM: the buffer TARGET+TBLFM, recalculated LAPS
times and aligned, must equal EXPECT+TBLFM.

Usage: extract-org-fixtures.py ORG_CHECKOUT REVISION > tests/fixtures/org-table-tests.json
"""
import json
import sys


class Sym(str):
    pass


def read_all(src):
    i = 0
    n = len(src)
    line_of = []
    out = []

    def skip_ws():
        nonlocal i
        while i < n:
            c = src[i]
            if c in " \t\r\n\f":
                i += 1
            elif c == ";":
                while i < n and src[i] != "\n":
                    i += 1
            else:
                break

    def read():
        nonlocal i
        skip_ws()
        if i >= n:
            raise EOFError
        c = src[i]
        start = i
        if c in "([":
            close = ")" if c == "(" else "]"
            i += 1
            items = []
            while True:
                skip_ws()
                if i < n and src[i] == close:
                    i += 1
                    break
                items.append(read())
            lst = List(items)
            lst.line = src.count("\n", 0, start) + 1
            return lst
        if c == '"':
            i += 1
            buf = []
            while src[i] != '"':
                if src[i] == "\\":
                    e = src[i + 1]
                    i += 2
                    if e == "n":
                        buf.append("\n")
                    elif e == "t":
                        buf.append("\t")
                    elif e == "\n":
                        pass
                    else:
                        buf.append(e)
                else:
                    buf.append(src[i])
                    i += 1
            i += 1
            return "".join(buf)
        if c == "'":
            i += 1
            return List([Sym("quote"), read()])
        if c in "`,":
            i += 1
            if i < n and src[i] == "@":
                i += 1
            return List([Sym("quasi"), read()])
        if c == "#" and src[i + 1] == "'":
            i += 2
            return List([Sym("function"), read()])
        if c == "?":
            i += 1
            if src[i] == "\\":
                i += 2
            else:
                i += 1
            return Sym("?char")
        j = i
        while j < n and src[j] not in " \t\r\n\f()[]\";'":
            if src[j] == "\\":
                j += 1
            j += 1
        tok = src[i:j]
        i = j
        try:
            return int(tok)
        except ValueError:
            return Sym(tok)

    while True:
        try:
            out.append(read())
        except EOFError:
            break
    return out


class List(list):
    line = 0


def evaluate(x, env):
    if isinstance(x, Sym):
        if x == "nil":
            return None
        if x in env:
            return env[x]
        raise KeyError(x)
    if isinstance(x, (str, int)):
        return x
    if isinstance(x, List) and x and x[0] == "concat":
        return "".join(evaluate(a, env) for a in x[1:])
    if isinstance(x, List) and x and x[0] == "quote":
        return Sym(x[1]) if isinstance(x[1], Sym) else x[1]
    raise ValueError(f"cannot evaluate {x!r}")


def walk(form, env, test, out):
    if not isinstance(form, List) or not form:
        return
    head = form[0]
    if head == "ert-deftest":
        test = str(form[1])
    if head in ("let", "let*") and isinstance(form[1], List):
        env = dict(env)
        for b in form[1]:
            if isinstance(b, List) and len(b) == 2:
                try:
                    env[b[0]] = evaluate(b[1], env)
                except (KeyError, ValueError):
                    pass
    if head == "org-test-table-target-expect":
        args = [evaluate(a, env) for a in form[1:]]
        target = args[0]
        expect = args[1] if len(args) > 1 and args[1] is not None else target
        laps = args[2] if len(args) > 2 else None
        tblfms = args[3:] or [""]
        out.append({"test": test, "line": form.line, "target": target, "expect": expect,
                    "laps": laps if not isinstance(laps, Sym) else str(laps), "tblfm": tblfms})
        return
    for sub in form[1:]:
        walk(sub, env, test, out)


def main():
    root, revision = sys.argv[1], sys.argv[2]
    path = root + "/testing/lisp/test-org-table.el"
    src = open(path, encoding="utf-8").read()
    forms = read_all(src)
    env = {}
    for f in forms:
        if isinstance(f, List) and f and f[0] in ("defconst", "defvar") and len(f) >= 3:
            try:
                env[f[1]] = evaluate(f[2], env)
            except (KeyError, ValueError):
                pass
    calls = []
    for f in forms:
        walk(f, env, None, calls)
    cases = []
    for c in calls:
        for k, tblfm in enumerate(c["tblfm"]):
            cases.append({
                "id": f"{c['test']}@{c['line']}#{k + 1}",
                "test": c["test"], "line": c["line"], "laps": c["laps"],
                "input": c["target"] + tblfm, "expected": c["expect"] + tblfm,
            })
    json.dump({"source": "org-mode testing/lisp/test-org-table.el", "revision": revision,
               "cases": cases}, sys.stdout, indent=1, ensure_ascii=False)
    sys.stdout.write("\n")


main()
