#!/usr/bin/env python3
"""Avisos de seguridad (RUSTSEC) contra el Cargo.lock de CensusIndex.

`cargo audit` no se puede instalar dentro del límite de tiempo del agente, así
que esto hace lo mismo con la base oficial clonada:

    git clone --depth 1 https://github.com/rustsec/advisory-db /tmp/advisory-db
    python tools/audit-advisories.py /tmp/advisory-db

Sale 0 si no hay avisos, 1 si hay alguno que afecte al lock.
Sólo stdlib: tomllib (Python 3.11+) y re.
"""
import pathlib
import re
import sys
import tomllib

ROOT = pathlib.Path(__file__).resolve().parent.parent


def parse_lock(path: pathlib.Path) -> dict[str, str]:
    """{nombre: versión} de cada paquete con versión en el lock."""
    pkgs: dict[str, str] = {}
    name = None
    for line in path.read_text(encoding="utf-8").splitlines():
        m = re.match(r'name = "([^"]+)"', line)
        if m:
            name = m.group(1)
            continue
        m = re.match(r'version = "([^"]+)"', line)
        if m and name:
            pkgs[name] = m.group(1)
            name = None
    return pkgs


def vkey(v: str) -> tuple:
    """Clave comparable: 1.2.3-pre.4 -> ((1,2,3), (0, 'pre', 4)) que ordena bien."""
    core, _, pre = v.partition("-")
    nums = []
    for part in core.split("."):
        digits = re.match(r"\d+", part)
        nums.append(int(digits.group()) if digits else 0)
    return (tuple(nums), pre or "~")


def cmp_ver(a: str, b: str) -> int:
    ka, kb = vkey(a), vkey(b)
    if ka < kb:
        return -1
    if ka > kb:
        return 1
    return 0


def satisfied(version: str, req: str) -> bool:
    """Soporta los operadores que usa la base: >=, >, <, <=, =, ^, ~ y '*'."""
    req = req.strip()
    if req in ("", "*"):
        return True
    for op in (">=", "<=", ">", "<", "=", "^", "~"):
        if req.startswith(op):
            want = req[len(op):].strip()
            c = cmp_ver(version, want)
            if op == ">=":
                return c >= 0
            if op == "<=":
                return c <= 0
            if op == ">":
                return c > 0
            if op == "<":
                return c < 0
            if op == "=":
                return c == 0
            if op == "^":
                # ^1.2.3: mismo número mayor (0.x: mismo menor)
                va, wa = vkey(version)[0], vkey(want)[0]
                if wa[0] != 0:
                    return va[0] == wa[0] and c >= 0
                return va[0] == 0 and va[1] == wa[1] and c >= 0
            if op == "~":
                va, wa = vkey(version)[0], vkey(want)[0]
                return va[:2] == wa[:2] and c >= 0
    return version == req


def parse_advisory(path: pathlib.Path) -> dict | None:
    text = path.read_text(encoding="utf-8", errors="replace")
    m = re.match(r"```toml\n(.*?)\n```", text, re.S)
    if not m:
        return None
    try:
        data = tomllib.loads(m.group(1))
    except tomllib.TOMLDecodeError:
        return None
    return data


def main() -> int:
    if len(sys.argv) < 2:
        print("uso: audit-advisories.py <ruta-a-advisory-db>")
        return 2
    db = pathlib.Path(sys.argv[1])
    pkgs = parse_lock(ROOT / "Cargo.lock")
    print(f"lock: {len(pkgs)} paquetes · base: {db}")

    hits = []
    revisados = 0
    for path in sorted(db.glob("crates/*/*.md")):
        adv = parse_advisory(path)
        if not adv:
            continue
        revisados += 1
        info = adv.get("advisory", {})
        name = info.get("package")
        if name not in pkgs:
            continue
        if info.get("withdrawn"):
            continue
        version = pkgs[name]
        versions = adv.get("versions", {})
        patched = versions.get("patched", [])
        unaffected = versions.get("unaffected", [])
        # Si no hay parche conocido, o nuestra versión no cae en ningún rango
        # parcheado, el aviso aplica.
        if any(satisfied(version, r) for r in patched):
            continue
        if any(r == f"={version}" or satisfied(version, r) and r.startswith("=") for r in unaffected):
            continue
        hits.append((info.get("id", "?"), name, version, info.get("informational", "")))

    print(f"avisos revisados: {revisados}")
    if not hits:
        print("sin avisos aplicables: OK")
        return 0
    print(f"\n{len(hits)} aviso(s) aplicables:")
    for ident, name, version, kind in hits:
        extra = f" [{kind}]" if kind else ""
        print(f"  {ident}  {name} {version}{extra}")
    return 1


if __name__ == "__main__":
    sys.exit(main())
