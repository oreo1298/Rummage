#!/usr/bin/env python3
"""Deterministic filesystem storm: random creates, writes, renames, moves, deletes, chmods,
symlinks and hard links under ROOT. Usage: storm.py ROOT OPS SEED"""
import os, random, shutil, sys

root, ops, seed = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
rnd = random.Random(seed)
names = ["a", "b", "Report", "café", "x y", "data", "é", "dot.d", ".hidden", "Ω", "k"]


def name():
    n = rnd.choice(names) + str(rnd.randrange(1000))
    if rnd.random() < 0.05:
        return os.fsencode(n) + b"\xff"  # not valid UTF-8
    return os.fsencode(n)


def dirs():
    out = [os.fsencode(root)]
    for d, sub, _ in os.walk(os.fsencode(root)):
        out.extend(os.path.join(d, s) for s in sub if not os.path.islink(os.path.join(d, s)))
    return out


def entries():
    out = []
    for d, sub, files in os.walk(os.fsencode(root)):
        out.extend(os.path.join(d, x) for x in sub + files)
    return out


all_dirs = dirs()
cached = entries()
for i in range(ops):
    if i % 100 == 0:
        all_dirs = dirs()
        cached = entries()
    op = rnd.random()
    try:
        if op < 0.25:
            p = os.path.join(rnd.choice(all_dirs), name())
            with open(p, "wb") as f:
                f.write(b"x" * rnd.randrange(5000))
        elif op < 0.35:
            p = os.path.join(rnd.choice(all_dirs), name())
            os.mkdir(p)
            all_dirs.append(p)
        elif op < 0.45:
            es = cached
            if es:
                p = rnd.choice(es)
                if os.path.isfile(p) and not os.path.islink(p):
                    with open(p, "ab") as f:
                        f.write(b"y" * rnd.randrange(3000))
        elif op < 0.60:
            es = cached
            if es:
                src = rnd.choice(es)
                dst_dir = rnd.choice(all_dirs)
                if not os.path.abspath(dst_dir).startswith(os.path.abspath(src) + b"/") and dst_dir != src:
                    os.rename(src, os.path.join(dst_dir, name()))
        elif op < 0.72:
            es = cached
            if es:
                p = rnd.choice(es)
                if os.path.isdir(p) and not os.path.islink(p):
                    shutil.rmtree(p)
                else:
                    os.unlink(p)
        elif op < 0.80:
            es = cached
            if es:
                p = rnd.choice(es)
                if not os.path.islink(p):
                    os.chmod(p, rnd.choice([0o755, 0o700, 0o644, 0o600, 0o711]))
        elif op < 0.88:
            os.symlink(b"target-" + name(), os.path.join(rnd.choice(all_dirs), name()))
        elif op < 0.93:
            es = [e for e in cached[:200] if os.path.isfile(e) and not os.path.islink(e)]
            if es:
                os.link(rnd.choice(es), os.path.join(rnd.choice(all_dirs), name()))
        else:
            # A burst: a new directory with many files (like unpacking an archive).
            d = os.path.join(rnd.choice(all_dirs), name())
            os.mkdir(d)
            for j in range(rnd.randrange(50, 300)):
                open(os.path.join(d, b"f%d" % j), "wb").close()
            all_dirs.append(d)
    except (FileExistsError, FileNotFoundError, NotADirectoryError, IsADirectoryError, OSError):
        pass
