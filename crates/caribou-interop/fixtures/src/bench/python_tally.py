# The Python caller in the interop benchmark. Each exported static method
# loops in Python so the harness measures one operation per iteration.

from bench.Bench import Bench
from bench.tally import Tally


class PythonTally:
    def __init__(self, value: float):
        self.value = value

    @staticmethod
    def add(x: float) -> float:
        return x + 1.0

    def bump(self, x: float) -> float:
        self.value += x
        return self.value

    @staticmethod
    def adder():
        return lambda x: x + 1.0

    @staticmethod
    def pythonStatic(n: int) -> float:
        total = 0.0
        for _ in range(n):
            total = PythonTally.add(total)
        return total

    @staticmethod
    def pythonMethod(n: int) -> float:
        tally = PythonTally(0.0)
        for _ in range(n):
            tally.bump(1.0)
        return tally.value

    @staticmethod
    def pythonGetter(n: int) -> float:
        tally = PythonTally(3.0)
        total = 0.0
        for _ in range(n):
            total += tally.value
        return total

    @staticmethod
    def pythonSetter(n: int) -> float:
        tally = PythonTally(0.0)
        for i in range(n):
            tally.value = float(i)
        return tally.value

    @staticmethod
    def pythonClosure(n: int) -> float:
        f = PythonTally.adder()
        total = 0.0
        for _ in range(n):
            total = f(total)
        return total

    @staticmethod
    def pythonNew(n: int) -> float:
        tally = PythonTally(0.0)
        for i in range(n):
            tally = PythonTally(float(i))
        return tally.value

    @staticmethod
    def haxeStatic(n: int) -> float:
        total = 0.0
        for _ in range(n):
            total = Bench.add(total)
        return total

    @staticmethod
    def haxeMethod(n: int) -> float:
        bench = Bench()
        for _ in range(n):
            bench.bump(1.0)
        return bench.v

    @staticmethod
    def haxeGetter(n: int) -> float:
        bench = Bench()
        bench.v = 3.0
        total = 0.0
        for _ in range(n):
            total += bench.v
        return total

    @staticmethod
    def haxeSetter(n: int) -> float:
        bench = Bench()
        for i in range(n):
            bench.v = float(i)
        return bench.v

    @staticmethod
    def haxeClosure(n: int) -> float:
        f = Bench.adder()
        total = 0.0
        for _ in range(n):
            total = f(total)
        return total

    @staticmethod
    def haxeNew(n: int) -> float:
        bench = Bench()
        for _ in range(n):
            bench = Bench()
        return bench.v

    @staticmethod
    def wrenStatic(n: int) -> float:
        total = 0.0
        for _ in range(n):
            total = Tally.add(total)
        return total

    @staticmethod
    def wrenMethod(n: int) -> float:
        tally = Tally(0.0)
        for _ in range(n):
            tally.bump(1.0)
        return tally.total

    @staticmethod
    def wrenGetter(n: int) -> float:
        tally = Tally(3.0)
        total = 0.0
        for _ in range(n):
            total += tally.total
        return total

    @staticmethod
    def wrenSetter(n: int) -> float:
        tally = Tally(0.0)
        for i in range(n):
            tally.total = float(i)
        return tally.total

    @staticmethod
    def wrenClosure(n: int) -> float:
        f = Tally.adder()
        total = 0.0
        for _ in range(n):
            total = f(total)
        return total

    @staticmethod
    def wrenNew(n: int) -> float:
        tally = Tally(0.0)
        for i in range(n):
            tally = Tally(float(i))
        return tally.total
