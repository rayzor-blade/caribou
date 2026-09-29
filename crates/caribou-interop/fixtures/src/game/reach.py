from math.Vec2 import Vec2
from math.Math import Math


def length() -> float:
    return Vec2(3, 4).len()


def inferred_length():
    return Vec2(3, 4).len()


def scaled(k: float) -> float:
    v = Vec2(3, 4)
    v.scale(k)
    return v.len()


def loud(s: str) -> str:
    return Math.shout(s)


def hypot() -> float:
    return Math.hypot(5, 12)


def measure(v) -> float:
    return v.len()


def missing() -> str:
    try:
        Math.nothing()
    except AttributeError as e:
        return str(e)
    return "reached"
