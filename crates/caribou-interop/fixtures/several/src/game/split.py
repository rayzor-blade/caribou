# Python using Lua's two results as a tuple, and giving Wren one of its
# own tuples, which Wren takes as a list.
from game.twice import qr
from game.show import Show


def split(a: int, b: int) -> int:
    q, r = qr(a, b)
    return q * 100 + r


# Wren has one number, a float.
def counted() -> float:
    return Show.count((1, 2, 3))


def pair(a: int, b: int) -> tuple[int, int]:
    return (a, b)
