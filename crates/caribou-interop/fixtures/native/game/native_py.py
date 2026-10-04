# The native bench's Python side (benches/native.rs): each function does
# one operation on the math plugin `n` times.
from math.Vec2 import Vec2
from math.Math import Math


def baseline(n: int) -> float:
    s = 0.0
    i = 0
    while i < n:
        s = s + 1.0
        i = i + 1
    return s


def statics(n: int) -> float:
    s = 0.0
    i = 0
    while i < n:
        s = s + Math.hypot(3.0, 4.0)
        i = i + 1
    return s


def methods(n: int) -> float:
    v = Vec2(3.0, 4.0)
    s = 0.0
    i = 0
    while i < n:
        s = s + v.len()
        i = i + 1
    return s


def getter(n: int) -> float:
    v = Vec2(3.0, 4.0)
    s = 0.0
    i = 0
    while i < n:
        s = s + v.x
        i = i + 1
    return s


def setter(n: int) -> float:
    v = Vec2(3.0, 4.0)
    i = 0
    while i < n:
        v.x = 1.0
        i = i + 1
    return v.x


def strings(n: int) -> float:
    s = 0.0
    i = 0
    while i < n:
        t = Math.stars(1)
        s = s + 1.0
        i = i + 1
    return s
