from cyc.wren_side import WrenSide


class PySide:
    def __init__(self, n: float):
        self.n = n

    def twice(self) -> float:
        return WrenSide.double(self.n)
