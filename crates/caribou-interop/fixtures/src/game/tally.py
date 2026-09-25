# A Python module of the interop tests, through Zyntax's Python frontend.
# Its functions are the module's own: Wren imports them by name,
# `import "game:tally" for score`. Its class is what Haxe sees of it,
# `game.tally.Tally`, as a Python class is the type a Python module
# declares.

def score(hits: int, misses: int) -> int:
    return hits * 10 - misses * 3

def weight(hits: float, factor: float) -> float:
    return hits * factor + 0.5

def perfect(hits: int, total: int) -> bool:
    return hits == total

def echo(name: str) -> str:
    return name

def greet(name: str) -> str:
    return "hi " + name

class Tally:
    def __init__(self, hits: int):
        self.hits = hits

    def total(self, misses: int) -> int:
        return score(self.hits, misses)
